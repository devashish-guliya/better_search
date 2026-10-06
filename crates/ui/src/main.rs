#![windows_subsystem = "windows"]
//! Unelevated search UI. All index access goes through bs_pipe::Client in `search`.

mod apps;
mod commands;
mod draw;
mod frecency;
mod rows;
mod search;
mod settings;
mod settings_page;
mod stats;
mod thumbs;
mod update;
mod winkey;
mod winsearch;

use std::collections::{HashMap, HashSet};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use bs_pipe::{Hit, Status};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush,
    DEFAULT_GUI_FONT, DT_CENTER, DT_PATH_ELLIPSIS, DT_RIGHT, DeleteDC, DeleteObject, EndPaint,
    FillRect, GetDC, GetMonitorInfoW, GetStockObject, HBRUSH, HFONT, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT, ReleaseDC, SRCCOPY,
    SelectObject, SetBkColor, SetTextColor,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, GetDriveTypeW, GetVolumeInformationW,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETMARGINS, ICC_LISTVIEW_CLASSES, ILC_COLOR32, ILD_TRANSPARENT,
    INITCOMMONCONTROLSEX, ImageList_Add, ImageList_Create, ImageList_Destroy, ImageList_Draw,
    ImageList_Remove, ImageList_ReplaceIcon, InitCommonControlsEx, LVCF_TEXT, LVCF_WIDTH,
    LVCOLUMNW, LVHITTESTINFO, LVIF_TEXT, LVIR_BOUNDS, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW,
    LVM_ENSUREVISIBLE, LVM_GETITEMRECT, LVM_GETNEXTITEM, LVM_HITTEST, LVM_INSERTCOLUMNW,
    LVM_REDRAWITEMS, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETIMAGELIST, LVM_SETITEMCOUNT,
    LVM_SETITEMSTATE, LVN_GETDISPINFOW, LVN_ITEMCHANGED, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_NOCOLUMNHEADER, LVS_OWNERDATA, LVS_OWNERDRAWFIXED, LVS_REPORT,
    LVS_SHAREIMAGELISTS, LVS_SHOWSELALWAYS, LVS_SINGLESEL, LVSIL_SMALL, MEASUREITEMSTRUCT,
    NM_DBLCLK, NMHDR, NMLVDISPINFOW, ODS_SELECTED, SetWindowTheme, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, RegisterHotKey, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    UnregisterHotKey, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RETURN, VK_RWIN,
    VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};

use draw::scale;
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    RemoveWindowSubclass, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
    SHGetFileInfoW, SetWindowSubclass, Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, EN_CHANGE, ES_AUTOHSCROLL, FindWindowW, GWLP_USERDATA,
    GetClientRect, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, IDC_ARROW, IDI_APPLICATION, IsWindowVisible, KillTimer,
    LoadCursorW, LoadIconW, MB_DEFBUTTON2, MB_ICONQUESTION, MB_YESNO, MF_STRING, MSG, MessageBoxW,
    MoveWindow, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SW_HIDE,
    SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOZORDER, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WINDOWPOS, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND,
    WM_HOTKEY, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_MEASUREITEM,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_NOTIFY, WM_PAINT, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETFOCUS, WM_SETFONT, WM_SETTINGCHANGE, WM_SIZE, WM_SYSCHAR, WM_SYSCOMMAND,
    WM_SYSKEYDOWN, WM_THEMECHANGED, WM_TIMER, WM_WINDOWPOSCHANGED, WNDCLASSW, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};
/// `WM_SYSCOMMAND`'s menu-open command, sent for Alt+Space and F10.
const SC_KEYMENU: usize = 0xf100;
/// Sent when the user changes the Windows accent colour.
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const WM_MOUSELEAVE: u32 = 0x02a3;
/// Static control styles: vertically centred, single-line text cut with an ellipsis.
const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;
/// The Yes answer from `MessageBoxW`; this windows-sys version does not export it.
const IDYES: i32 = 6;

const CLASS: &str = "BetterSearchWindow";
const EDIT_ID: usize = 101;
const LIST_ID: usize = 102;
const STATUS_ID: usize = 103;
const TRAY_MESSAGE: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 2;
/// A second launch asks the running window to come forward instead of opening again.
const WM_SHOW_PANEL: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 7;
/// The update worker asks the window to replace itself with the installed build.
const WM_RESTART: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 8;
/// The setup program asks the window to exit before it removes the files, so the program
/// image is released instead of being left running from a folder that no longer exists.
const WM_QUIT_PANEL: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 9;
const HOTKEY_ID: i32 = 1;
const MENU_OPEN: usize = 201;
const MENU_SETTINGS: usize = 202;
const MENU_PAUSE: usize = 203;
const MENU_QUIT: usize = 204;
const MENU_UPDATE: usize = 205;
const MENU_RESULT_OPEN: usize = 210;
const MENU_RESULT_FOLDER: usize = 211;
const MENU_RESULT_COPY: usize = 212;
const RETRY_TIMER: usize = 3;
const NOTICE_TIMER: usize = 4;
/// Result icons and previews, in pixels at 96 DPI.
const ICON_SIZE: i32 = 32;
/// Pictures kept before the image list starts over.
const MAX_IMAGES: usize = 600;
/// `EM_SETMARGINS` flags for the search box's inner text padding.
const EC_LEFTMARGIN: usize = 0x0001;
const EC_RIGHTMARGIN: usize = 0x0002;
const EM_SETCUEBANNER: u32 = 0x1501;
const KEY_HINTS: &str = "Ctrl+Enter  show in folder";
/// Shown once, the first time the panel opens after the first scan has finished.
const OFFER_TITLE: &str = "Turn off Windows indexing?";

/// The one-time offer's wording: what Windows' file indexing is, in plain words, what
/// better_search already does instead, and what turning it off would cost. No
/// numbers: they belong in Settings, where the user can check them.
fn offer_text() -> &'static str {
    "Do you want to turn off the Windows indexing of your files?\n\n\
     Windows search keeps a list of your files so it can find them by name, and it \
     builds and stores that list in the background while you use your PC. \
     better_search already keeps its own, much smaller and faster list - that is what \
     you are searching right now.\n\n\
     Turning indexing off frees that space and background work. Programs that search \
     inside documents, such as Outlook, would search more slowly, and you can turn \
     indexing back on any time in better_search Settings."
}

/// What better_search keeps on disk: the saved index and its log. Not the size of the
/// programs, because Windows search's figure is its index too, and those are the two that
/// grow with the number of files.
fn our_index_disk(sizes: &bs_pipe::StatsReply) -> Option<u64> {
    let parts = [sizes.snapshot_disk, sizes.log_disk];
    parts
        .iter()
        .all(Option::is_some)
        .then(|| parts.into_iter().flatten().sum())
}

/// The Settings line: what this PC costs, and where the space goes.
fn sizes_text(reading: Option<&stats::Reading>) -> String {
    let Some(stats::Reading::Sizes(sizes)) = reading else {
        return match reading {
            Some(_) => "Measuring once the first scan finishes…".into(),
            None => "Measuring…".into(),
        };
    };
    let Some(index) = our_index_disk(sizes) else {
        return "Measuring…".into();
    };
    let mut text = format!(
        "Using {} of memory and keeping a {} index",
        sizes
            .service_working_set
            .map_or_else(|| "?".into(), stats::size),
        stats::size(index)
    );
    match sizes.entries {
        Some(entries) => {
            text.push_str(&format!(" for {} files and folders", stats::count(entries)))
        }
        None => text.push_str(" for the files and folders on your drives"),
    }
    text.push_str(
        ". The index lives in C:\\ProgramData\\better_search and grows with the number of files.",
    );
    text
}

/// Default panel geometry: a square whose side is 70% of the work area height,
/// centred vertically and flush with the right screen edge.
fn panel_rect(work: RECT) -> (i32, i32, i32, i32) {
    let work_width = work.right - work.left;
    let work_height = work.bottom - work.top;
    let side = (work_height * 7 / 10).max(1).min(work_width);
    let x = work.right - side;
    let y = work.top + (work_height - side) / 2;
    (x, y, side, side)
}

struct App {
    edit: HWND,
    list: HWND,
    status_label: HWND,
    sender: Option<Sender<search::Request>>,
    results: Option<Receiver<search::ResultMessage>>,
    serial: u64,
    hits: Vec<Hit>,
    /// What the list shows, in order: section headings and hits.
    rows: Vec<rows::Row>,
    /// The row of each hit.
    hit_rows: Vec<usize>,
    names: Vec<Vec<u16>>,
    /// Which letters of each name matched the search.
    marks: Vec<Vec<bool>>,
    /// Second lines: what an app is, or the folder a file is in.
    details: Vec<String>,
    hover_row: Option<usize>,
    status: String,
    /// A short confirmation shown in the footer instead of the status.
    notice: Option<String>,
    last_text: String,
    /// Type icons by extension, as image list slots.
    icon_cache: HashMap<String, i32>,
    /// The file's own picture by path, as image list slots.
    path_icons: HashMap<String, i32>,
    /// Paths sent to the thumbnail worker for the current result list.
    requested: HashSet<String>,
    image_list: isize,
    icon_size: i32,
    image_count: usize,
    thumb_sender: Option<Sender<thumbs::Request>>,
    thumb_results: Option<Receiver<thumbs::Done>>,
    thumb_generation: Arc<AtomicU64>,
    apps: apps::Apps,
    /// Names of the packaged apps in the current results, by path.
    app_names: HashMap<String, String>,
    /// Row names of the drives seen so far (label and letter), by root path.
    drive_names: HashMap<String, String>,
    settings: settings::Settings,
    history: frecency::History,
    paused: bool,
    /// Ctrl+H: also show matches inside system, app-data, and program folders.
    /// Per session, so the window always starts with the tidy view.
    include_system: bool,
    hotkey_registered: bool,
    settings_open: bool,
    /// The settings page's lines and rects, rebuilt when its inputs change.
    settings_page: settings_page::Page,
    /// The settings row under the mouse, and the keyboard's row.
    settings_hover: Option<usize>,
    settings_cursor: Option<usize>,
    /// How far the settings page is scrolled up (its wheel scrolling).
    settings_scroll: i32,
    /// The hotkey band is being edited: the next keys become the hotkey.
    capturing: bool,
    /// A hint about the capture, shown in the band until the next key.
    capture_note: Option<&'static str>,
    light: Option<bool>,
    background: HBRUSH,
    surface_brush: HBRUSH,
    fonts: Option<draw::Fonts>,
    colors: draw::Palette,
    /// The search field and the footer, drawn by the window itself.
    field: RECT,
    footer: RECT,
    show_hints: std::cell::Cell<bool>,
    taskbar_message: u32,
    /// The settings button drawn at the right end of the search field.
    gear: RECT,
    /// The last sizes the service reported. `None` until it has been asked.
    sizes: Option<stats::Reading>,
    /// The sizes worker, kept so a result is never read from a dead channel.
    sizes_results: Option<Receiver<stats::Reading>>,
    sizes_stop: Option<Sender<()>>,
    /// An offer to replace Windows search that is owed, but not yet shown: the panel
    /// was not on screen and idle when the offer was due.
    offer_pending: bool,
}

impl App {
    fn new() -> Self {
        Self {
            edit: null_mut(),
            list: null_mut(),
            status_label: null_mut(),
            sender: None,
            results: None,
            serial: 0,
            hits: Vec::new(),
            rows: Vec::new(),
            hit_rows: Vec::new(),
            names: Vec::new(),
            marks: Vec::new(),
            details: Vec::new(),
            hover_row: None,
            status: String::new(),
            notice: None,
            last_text: String::new(),
            icon_cache: HashMap::new(),
            path_icons: HashMap::new(),
            requested: HashSet::new(),
            image_list: 0,
            icon_size: 0,
            image_count: 0,
            thumb_sender: None,
            thumb_results: None,
            thumb_generation: Arc::new(AtomicU64::new(0)),
            apps: apps::Apps::default(),
            app_names: HashMap::new(),
            drive_names: HashMap::new(),
            settings: settings::Settings::load(),
            history: frecency::History::load(),
            paused: false,
            include_system: false,
            hotkey_registered: false,
            settings_open: false,
            settings_page: settings_page::Page {
                lines: Vec::new(),
                hotkey: RECT::default(),
                back: RECT::default(),
                title: RECT::default(),
                content: 0,
            },
            settings_hover: None,
            settings_cursor: None,
            settings_scroll: 0,
            capturing: false,
            capture_note: None,
            light: None,
            background: null_mut(),
            surface_brush: null_mut(),
            fonts: None,
            colors: draw::palette(true, settings::accent_color()),
            field: RECT::default(),
            footer: RECT::default(),
            show_hints: std::cell::Cell::new(true),
            taskbar_message: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
            gear: RECT::default(),
            sizes: None,
            sizes_results: None,
            sizes_stop: None,
            offer_pending: false,
        }
    }

    fn edit_text(&self) -> String {
        let len = unsafe { GetWindowTextLengthW(self.edit) };
        let mut text = vec![0u16; len.max(0) as usize + 1];
        unsafe { GetWindowTextW(self.edit, text.as_mut_ptr(), text.len() as i32) };
        String::from_utf16_lossy(&text[..len.max(0) as usize])
    }

    fn query(&mut self, hwnd: HWND) {
        let query = self.edit_text();
        self.last_text = query.clone();
        self.serial = self.serial.wrapping_add(1);
        unsafe { KillTimer(hwnd, RETRY_TIMER) };
        self.apps.refresh_if_stale();
        self.clear_results();
        if query.trim().is_empty() {
            self.status = String::new();
        } else if self.last_text.len() + 4 > bs_pipe::MAX_REQUEST {
            self.status = "The search is too long".into();
        } else if self.paused {
            self.status = "Search paused from the tray".into();
        } else {
            self.status = "Searching…".into();
            if let Some(sender) = &self.sender {
                let _ = sender.send(search::Request {
                    serial: self.serial,
                    text: self.last_text.clone(),
                    include_system: self.include_system,
                });
            }
        }
        self.update_title(hwnd);
    }

    fn clear_results(&mut self) {
        self.hits.clear();
        self.rows.clear();
        self.hit_rows.clear();
        self.names.clear();
        self.marks.clear();
        self.details.clear();
        self.hover_row = None;
        if !self.list.is_null() {
            unsafe { SendMessageW(self.list, LVM_SETITEMCOUNT, 0, 0) };
        }
    }

    fn apply_result(&mut self, hwnd: HWND, result: search::ResultMessage) {
        if result.serial != self.serial {
            return;
        }
        self.clear_results();
        let mut retry = false;
        match result.outcome {
            search::Outcome::Reply(reply) => match reply.status {
                Status::Ok => {
                    let mut reply = reply;
                    let shortcut_names: HashSet<String> = reply
                        .hits
                        .iter()
                        .filter(|hit| is_app(hit))
                        .map(|hit| display_name(hit).to_lowercase())
                        .collect();
                    // A packaged app that also has a Start Menu shortcut is shown once.
                    let store: Vec<(Hit, String)> = self
                        .apps
                        .matches(&self.last_text)
                        .into_iter()
                        .filter(|(_, name)| !shortcut_names.contains(&name.to_lowercase()))
                        .chain(commands::matching(&self.last_text))
                        .collect();
                    let shown = reply.total_matches + store.len() as u32;
                    let hidden = reply.hidden_matches;
                    self.app_names.clear();
                    for (hit, name) in store {
                        self.app_names.insert(hit.path.clone(), name);
                        reply.hits.push(hit);
                    }
                    // Drives keep their names: the label of the volume, read once.
                    for hit in &reply.hits {
                        if let Some(letter) = drive_letter(&hit.path)
                            && !self.drive_names.contains_key(&hit.path)
                        {
                            self.drive_names
                                .insert(hit.path.clone(), drive_row_name(letter));
                        }
                    }
                    self.status = status_text(shown, hidden);
                    if self.include_system {
                        self.status.push_str(" · showing all");
                    }
                    let now = frecency::now();
                    let use_history = self.settings.history && !self.history.is_empty();
                    // Stable, so equal scores keep the service's order.
                    reply.hits.sort_by_cached_key(|hit| {
                        let boost = if use_history {
                            self.history.boost(&hit.path, now)
                        } else {
                            0
                        };
                        std::cmp::Reverse(hit.score + boost)
                    });
                    // Apps and Settings pages are few and always listed; files fill the
                    // rest of the list.
                    let mut files = 0;
                    reply.hits.retain(|hit| {
                        let extra = self.app_names.contains_key(&hit.path);
                        files += usize::from(!extra);
                        extra || files <= search::LIMIT as usize
                    });
                    let (hits, rows) = rows::group(reply.hits, kind_of);
                    let names: Vec<String> = hits.iter().map(|hit| self.name_of(hit)).collect();
                    self.marks = names
                        .iter()
                        .map(|name| rows::highlight(name, &self.last_text))
                        .collect();
                    self.names = names
                        .iter()
                        .map(|name| name.encode_utf16().collect())
                        .collect();
                    self.details = hits.iter().map(describe).collect();
                    self.hit_rows = vec![0; hits.len()];
                    for (row, entry) in rows.iter().enumerate() {
                        if let rows::Row::Hit(index) = *entry {
                            self.hit_rows[index] = row;
                        }
                    }
                    self.rows = rows;
                    // Pictures still queued for the previous list are not needed now.
                    self.thumb_generation.fetch_add(1, Ordering::Relaxed);
                    self.requested.clear();
                    self.hits = hits;
                    unsafe {
                        SendMessageW(self.list, LVM_SETITEMCOUNT, self.rows.len(), 0);
                    }
                    self.fit_column();
                    if !self.hits.is_empty() {
                        self.select(0);
                    }
                }
                Status::Loading => {
                    self.status =
                        "Indexing your drives; results appear when the first scan finishes".into();
                    retry = true;
                }
                Status::Denied => self.status = "Access denied by the search service".into(),
                Status::BadRequest => self.status = "The service rejected this search".into(),
            },
            search::Outcome::Unavailable => {
                self.status = "Service not running. Start bs-service to search.".into();
                retry = true;
            }
            search::Outcome::Denied => {
                self.status = "Access denied: the service or pipe could not be verified".into()
            }
            search::Outcome::Error(err) => self.status = format!("Search failed: {err}"),
        }
        if retry && unsafe { IsWindowVisible(hwnd) } != 0 {
            unsafe { SetTimer(hwnd, RETRY_TIMER, 1200, None) };
        }
        self.update_title(hwnd);
    }

    fn name_of(&self, hit: &Hit) -> String {
        self.app_names
            .get(&hit.path)
            .or_else(|| self.drive_names.get(&hit.path))
            .cloned()
            .unwrap_or_else(|| display_name(hit))
    }

    fn update_title(&self, hwnd: HWND) {
        // The settings page keeps its own title; the sizes replies must not reset it.
        let title = if self.settings_open {
            "better_search  ·  Settings"
        } else {
            "better_search"
        };
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(hwnd, wide(title).as_ptr())
        };
        self.refresh_status();
    }

    /// The selected list row, a heading or a hit.
    fn selected_row(&self) -> Option<usize> {
        let row =
            unsafe { SendMessageW(self.list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as _) };
        (row >= 0 && (row as usize) < self.rows.len()).then_some(row as usize)
    }

    /// The selected hit.
    fn selected(&self) -> Option<usize> {
        match self.rows.get(self.selected_row()?) {
            Some(&rows::Row::Hit(index)) => Some(index),
            _ => None,
        }
    }

    fn select(&self, index: usize) {
        let Some(&row) = self.hit_rows.get(index) else {
            return;
        };
        let clear = LVITEMW {
            state: 0,
            stateMask: LVIS_SELECTED | LVIS_FOCUSED,
            ..Default::default()
        };
        let select = LVITEMW {
            state: LVIS_SELECTED | LVIS_FOCUSED,
            stateMask: LVIS_SELECTED | LVIS_FOCUSED,
            ..Default::default()
        };
        // The first hit of a section scrolls its heading into view too.
        if row > 0 && matches!(self.rows[row - 1], rows::Row::Section(_)) {
            unsafe { SendMessageW(self.list, LVM_ENSUREVISIBLE, row - 1, 0) };
        }
        unsafe {
            SendMessageW(
                self.list,
                LVM_SETITEMSTATE,
                usize::MAX,
                (&clear as *const LVITEMW) as _,
            );
            SendMessageW(
                self.list,
                LVM_SETITEMSTATE,
                row,
                (&select as *const LVITEMW) as _,
            );
            SendMessageW(self.list, LVM_ENSUREVISIBLE, row, 0);
        }
    }

    /// Moves the selection `step` hits up or down, skipping headings.
    fn step_selection(&self, down: bool) {
        let current = self.selected().unwrap_or(0);
        let next = if down {
            (current + 1).min(self.hits.len().saturating_sub(1))
        } else {
            current.saturating_sub(1)
        };
        self.select(next);
    }

    /// The single column spans the list, whether or not it has a scroll bar.
    fn fit_column(&self) {
        // The client width excludes a vertical scroll bar, so no horizontal one appears.
        let mut client = RECT::default();
        unsafe {
            GetClientRect(self.list, &mut client);
            SendMessageW(
                self.list,
                windows_sys::Win32::UI::Controls::LVM_SETCOLUMNWIDTH,
                0,
                client.right as isize,
            )
        };
    }

    fn row_rect(&self, row: usize) -> RECT {
        let mut rect = RECT {
            left: LVIR_BOUNDS as i32,
            ..Default::default()
        };
        unsafe { SendMessageW(self.list, LVM_GETITEMRECT, row, (&raw mut rect) as isize) };
        rect
    }

    fn redraw_row(&self, row: Option<usize>) {
        if let Some(row) = row {
            unsafe { SendMessageW(self.list, LVM_REDRAWITEMS, row, row as isize) };
        }
    }

    /// The row under a list-client point.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let mut info = LVHITTESTINFO {
            pt: POINT { x, y },
            ..Default::default()
        };
        let row = unsafe { SendMessageW(self.list, LVM_HITTEST, 0, (&raw mut info) as isize) };
        (row >= 0 && (row as usize) < self.rows.len()).then_some(row as usize)
    }

    /// "Show in folder" and "Copy path" buttons at the right end of a hit row.
    fn row_buttons(&self, row: &RECT) -> [RECT; 2] {
        let size = scale(self.list, 30);
        let gap = scale(self.list, draw::SPACE_S);
        let right = row.right - scale(self.list, draw::SPACE_M);
        let top = row.top + (row.bottom - row.top - size) / 2;
        let copy = RECT {
            left: right - size,
            top,
            right,
            bottom: top + size,
        };
        let folder = RECT {
            left: copy.left - gap - size,
            right: copy.left - gap,
            ..copy
        };
        [folder, copy]
    }

    fn set_hover(&mut self, row: Option<usize>) {
        if self.hover_row != row {
            let old = self.hover_row;
            self.hover_row = row;
            self.redraw_row(old);
            self.redraw_row(row);
        }
    }

    fn show_notice(&mut self, hwnd: HWND, text: &str) {
        self.notice = Some(text.into());
        self.refresh_status();
        unsafe { SetTimer(hwnd, NOTICE_TIMER, 1600, None) };
    }

    /// Draws one list row: a section heading, or a hit as icon, name and detail line,
    /// with a rounded highlight when hovered or selected.
    fn draw_row(&mut self, item: &DRAWITEMSTRUCT) {
        let row = item.itemID as usize;
        let Some(&entry) = self.rows.get(row) else {
            return;
        };
        let Some(fonts) = &self.fonts else { return };
        let (name_font, bold_font, detail_font, heading_font, glyph_font) = (
            fonts.name,
            fonts.name_bold,
            fonts.detail,
            fonts.heading,
            fonts.glyph,
        );
        let colors = self.colors;
        let bounds = item.rcItem;
        let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
        if width <= 0 || height <= 0 {
            return;
        }
        let list = self.list;
        let s = |v: i32| scale(list, v);
        // Rows are drawn off screen and copied in one step so they never flicker.
        let screen = item.hDC;
        let dc = unsafe { CreateCompatibleDC(screen) };
        let bitmap = unsafe { CreateCompatibleBitmap(screen, width, height) };
        let old_bitmap = unsafe { SelectObject(dc, bitmap) };
        let local = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        unsafe { FillRect(dc, &local, self.background) };
        match entry {
            rows::Row::Section(kind) => {
                let label = RECT {
                    left: s(draw::SPACE_L),
                    top: height / 2,
                    right: width - s(draw::SPACE_L),
                    bottom: height - s(draw::SPACE_S),
                };
                draw::text(dc, kind.label(), &label, heading_font, colors.secondary, 0);
            }
            rows::Row::Hit(index) => {
                let selected = item.itemState & ODS_SELECTED != 0;
                let hovered = self.hover_row == Some(row);
                let card = RECT {
                    left: s(draw::SPACE_S),
                    top: s(1),
                    right: width - s(draw::SPACE_S),
                    bottom: height - s(1),
                };
                if selected || hovered {
                    let fill = if selected {
                        colors.selected
                    } else {
                        colors.hover
                    };
                    draw::fill_round(dc, &card, s(draw::RADIUS), fill);
                }
                if selected {
                    // Windows 11's selection mark: a short accent pill at the left.
                    let pill = draw::centered(
                        &RECT {
                            left: card.left,
                            right: card.left + s(3),
                            ..card
                        },
                        s(18),
                    );
                    draw::fill_round(dc, &pill, s(2), colors.accent);
                }
                let icon = self.icon(index);
                let icon_size = self.icon_size;
                let icon_left = s(draw::SPACE_L + draw::SPACE_S);
                if icon >= 0 {
                    unsafe {
                        ImageList_Draw(
                            self.image_list,
                            icon,
                            dc,
                            icon_left,
                            (height - icon_size) / 2,
                            ILD_TRANSPARENT,
                        )
                    };
                }
                let text_left = icon_left + icon_size + s(draw::SPACE_L);
                let mut text_right = width - s(draw::SPACE_L);
                if selected || hovered {
                    let buttons = self.row_buttons(&local);
                    for (rect, glyph) in buttons.iter().zip([draw::GLYPH_FOLDER, draw::GLYPH_COPY])
                    {
                        draw::text(dc, glyph, rect, glyph_font, colors.secondary, DT_CENTER);
                    }
                    text_right = buttons[0].left - s(draw::SPACE_M);
                }
                // Name and detail form one block, centred in the row.
                let name_height = draw::line_height(dc, name_font);
                let detail_height = draw::line_height(dc, detail_font);
                // Line heights include internal leading, so the lines overlap a little.
                let top = (height - name_height - detail_height + s(2)) / 2;
                let name_rect = RECT {
                    left: text_left,
                    top,
                    right: text_right,
                    bottom: top + name_height,
                };
                draw::marked_text(
                    dc,
                    &self.names[index],
                    &self.marks[index],
                    &name_rect,
                    (name_font, bold_font),
                    colors.text,
                );
                let detail_rect = RECT {
                    left: text_left,
                    top: name_rect.bottom - s(2),
                    right: text_right,
                    bottom: name_rect.bottom - s(2) + detail_height,
                };
                draw::text(
                    dc,
                    &self.details[index],
                    &detail_rect,
                    detail_font,
                    colors.secondary,
                    DT_PATH_ELLIPSIS,
                );
            }
        }
        unsafe {
            BitBlt(
                screen,
                bounds.left,
                bounds.top,
                width,
                height,
                dc,
                0,
                0,
                SRCCOPY,
            );
            SelectObject(dc, old_bitmap);
            DeleteObject(bitmap);
            DeleteDC(dc);
        }
    }

    /// Mouse presses on a row: its buttons act (if `act`), headings ignore clicks.
    /// Returns whether the press was handled here.
    fn click(&mut self, hwnd: HWND, x: i32, y: i32, act: bool) -> bool {
        let Some(row) = self.row_at(x, y) else {
            return false;
        };
        let rows::Row::Hit(index) = self.rows[row] else {
            return true;
        };
        let rect = self.row_rect(row);
        let local = RECT {
            left: 0,
            top: 0,
            right: rect.right - rect.left,
            bottom: rect.bottom - rect.top,
        };
        let [folder, copy] = self.row_buttons(&local);
        let point = (x - rect.left, y - rect.top);
        let inside = |r: &RECT| {
            point.0 >= r.left && point.0 < r.right && point.1 >= r.top && point.1 < r.bottom
        };
        let on_button = inside(&folder) || inside(&copy);
        if !on_button || !act {
            return on_button;
        }
        if inside(&folder) {
            self.select(index);
            self.open_selected(hwnd, true);
            true
        } else if inside(&copy) {
            self.select(index);
            self.copy_selected(hwnd);
            self.show_notice(hwnd, "Path copied");
            true
        } else {
            false
        }
    }

    /// Image list slot for row `index`: the file's own picture once the thumbnail
    /// worker has made it, a type icon until then.
    fn icon(&mut self, index: usize) -> i32 {
        let size = scale(self.list, ICON_SIZE);
        if self.image_list == 0 || self.icon_size != size {
            self.reset_images(size);
        }
        let hit = &self.hits[index];
        let source = if commands::is_command(&hit.path) {
            commands::ICON_SOURCE
        } else {
            &hit.path
        };
        if let Some(&slot) = self.path_icons.get(source) {
            return slot;
        }
        if self.requested.insert(source.to_owned())
            && let Some(sender) = &self.thumb_sender
        {
            let _ = sender.send(thumbs::Request {
                generation: self.thumb_generation.load(Ordering::Relaxed),
                path: source.to_owned(),
                size,
            });
        }
        self.type_icon(index)
    }

    fn type_icon(&mut self, index: usize) -> i32 {
        let hit = &self.hits[index];
        let name = hit.path.rsplit('\\').next().unwrap_or(&hit.path);
        let extension = if hit.path.starts_with(apps::PREFIX) {
            Some("exe")
        } else {
            (!hit.is_dir)
                .then(|| name.rsplit_once('.').map(|(_, ext)| ext))
                .flatten()
        };
        let key = if let Some(letter) = drive_letter(&hit.path) {
            format!("<drive {letter}>")
        } else if hit.is_dir {
            "<folder>".into()
        } else {
            extension.map_or_else(|| "<file>".into(), str::to_ascii_lowercase)
        };
        if let Some(&icon) = self.icon_cache.get(&key) {
            return icon;
        }
        let mut info = SHFILEINFOW::default();
        let flags = SHGFI_ICON | SHGFI_LARGEICON;
        // A drive root shows the shell's own icon for it; other type icons come from
        // attributes, not actual disk I/O.
        let found = if let Some(letter) = drive_letter(&hit.path) {
            let root: Vec<u16> = format!("{letter}\\").encode_utf16().chain([0]).collect();
            unsafe {
                SHGetFileInfoW(
                    root.as_ptr(),
                    0,
                    &mut info,
                    size_of::<SHFILEINFOW>() as u32,
                    flags,
                )
            }
        } else {
            let fake = if hit.is_dir {
                wide("folder")
            } else if let Some(extension) = extension {
                wide(&format!("file.{extension}"))
            } else {
                wide("file")
            };
            let attrs = if hit.is_dir {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            };
            unsafe {
                SHGetFileInfoW(
                    fake.as_ptr(),
                    attrs,
                    &mut info,
                    size_of::<SHFILEINFOW>() as u32,
                    flags | SHGFI_USEFILEATTRIBUTES,
                )
            }
        };
        let slot = if found != 0 && !info.hIcon.is_null() {
            let slot = unsafe { ImageList_ReplaceIcon(self.image_list, -1, info.hIcon) };
            unsafe { DestroyIcon(info.hIcon) };
            self.image_count += 1;
            slot
        } else {
            -1
        };
        self.icon_cache.insert(key, slot);
        slot
    }

    /// Starts an empty image list of `size` pixels, after a DPI change or when the
    /// list has grown large.
    fn reset_images(&mut self, size: i32) {
        if self.image_list != 0 && self.icon_size == size {
            unsafe { ImageList_Remove(self.image_list, -1) };
        } else {
            let old = self.image_list;
            self.image_list = unsafe { ImageList_Create(size, size, ILC_COLOR32, 64, 64) };
            self.icon_size = size;
            unsafe {
                SendMessageW(
                    self.list,
                    LVM_SETIMAGELIST,
                    LVSIL_SMALL as _,
                    self.image_list,
                )
            };
            if old != 0 {
                unsafe { ImageList_Destroy(old) };
            }
        }
        self.image_count = 0;
        self.icon_cache.clear();
        self.path_icons.clear();
        self.requested.clear();
        // Pictures made for the old list would land in the wrong slots.
        self.thumb_generation.fetch_add(1, Ordering::Relaxed);
    }

    fn thumbnails_ready(&mut self) {
        let Some(done) = &self.thumb_results else {
            return;
        };
        let done: Vec<thumbs::Done> = done.try_iter().collect();
        let mut added = false;
        for picture in done {
            if picture.bitmap.is_null() {
                continue;
            }
            if picture.size == self.icon_size && self.requested.contains(&picture.path) {
                let slot = unsafe { ImageList_Add(self.image_list, picture.bitmap, null_mut()) };
                if slot >= 0 {
                    self.image_count += 1;
                    self.path_icons.insert(picture.path, slot);
                    added = true;
                }
            }
            unsafe { DeleteObject(picture.bitmap) };
        }
        if self.image_count > MAX_IMAGES {
            self.reset_images(self.icon_size);
        }
        if added || self.image_count == 0 {
            unsafe { InvalidateRect(self.list, null(), 0) };
        }
    }

    fn open_selected(&mut self, hwnd: HWND, folder: bool) {
        let Some(index) = self.selected() else { return };
        // Showing a file in its folder is also the user picking that file.
        if self.settings.history {
            self.history.record(&self.hits[index].path, frecency::now());
            let _ = self.history.save();
        }
        let hit = &self.hits[index];
        if commands::is_power(&hit.path) {
            if !folder {
                commands::run(hwnd, &hit.path);
            }
            return;
        }
        let (file, args) = if folder && commands::is_command(&hit.path) {
            (wide("ms-settings:"), wide(""))
        } else if folder && hit.path.starts_with(apps::PREFIX) {
            // A packaged app has no folder of its own; show it among all apps.
            (wide("explorer.exe"), wide("shell:AppsFolder"))
        } else if folder {
            // File names cannot contain quotes, so this is safe for explorer's arguments.
            (
                wide("explorer.exe"),
                wide(&format!("/select,\"{}\"", hit.path)),
            )
        } else {
            (wide(&hit.path), Vec::new())
        };
        let args = if folder { args.as_ptr() } else { null() };
        unsafe { ShellExecuteW(hwnd, null(), file.as_ptr(), args, null(), SW_SHOW) };
    }

    fn copy_selected(&self, hwnd: HWND) {
        use windows_sys::Win32::Foundation::GlobalFree;
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        };
        use windows_sys::Win32::System::Memory::{
            GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
        };
        const CF_UNICODETEXT: u32 = 13;
        let Some(index) = self.selected() else { return };
        let value = wide(&self.hits[index].path);
        unsafe {
            if OpenClipboard(hwnd) == 0 {
                return;
            }
            let memory = GlobalAlloc(GMEM_MOVEABLE, value.len() * 2);
            if !memory.is_null() {
                let dest = GlobalLock(memory) as *mut u16;
                if !dest.is_null() {
                    std::ptr::copy_nonoverlapping(value.as_ptr(), dest, value.len());
                    GlobalUnlock(memory);
                    if EmptyClipboard() == 0 || SetClipboardData(CF_UNICODETEXT, memory).is_null() {
                        GlobalFree(memory);
                    }
                } else {
                    GlobalFree(memory);
                }
            }
            CloseClipboard();
        }
    }

    fn register_hotkey(&mut self, hwnd: HWND) {
        self.hotkey_registered =
            unsafe { RegisterHotKey(hwnd, HOTKEY_ID, self.settings.modifiers, self.settings.key) }
                != 0;
        if !self.hotkey_registered {
            self.status = "Hotkey unavailable; change it in Settings or use the tray".into();
            self.update_title(hwnd);
        }
    }

    fn show_settings(&mut self, hwnd: HWND) {
        self.settings_open = true;
        self.settings_cursor = None;
        self.settings_hover = None;
        self.capturing = false;
        self.capture_note = None;
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(self.edit, SW_HIDE);
            ShowWindow(self.list, SW_HIDE);
            ShowWindow(self.status_label, SW_HIDE);
            SetForegroundWindow(hwnd);
            // The page itself takes the keys: arrows move, Enter activates, Esc goes back.
            SetFocus(hwnd);
        }
        self.rebuild_page(hwnd);
        let title = wide("better_search  ·  Settings");
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(hwnd, title.as_ptr())
        };
    }

    fn show_search(&mut self, hwnd: HWND) {
        self.settings_open = false;
        self.settings_hover = None;
        self.settings_cursor = None;
        unsafe {
            ShowWindow(self.edit, SW_SHOW);
            ShowWindow(self.list, SW_SHOW);
            ShowWindow(self.status_label, SW_SHOW);
            SetFocus(self.edit);
        }
        self.layout(hwnd);
        self.update_title(hwnd);
    }

    fn open_panel(&mut self, hwnd: HWND) {
        let hidden = unsafe { IsWindowVisible(hwnd) } == 0;
        if self.settings_open {
            self.show_search(hwnd);
        }
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            SetFocus(self.edit);
        }
        if hidden && !self.last_text.trim().is_empty() {
            self.query(hwnd);
        }
        if hidden {
            self.offer_windows_search(hwnd);
        }
    }

    fn hide_panel(&mut self, hwnd: HWND) {
        unsafe {
            KillTimer(hwnd, RETRY_TIMER);
            ShowWindow(hwnd, SW_HIDE);
        }
    }

    /// Rebuilds the settings page for the current settings and service state. Called
    /// when the page opens, when a row changes something, and when the sizes reading
    /// or the window's size arrives.
    fn rebuild_page(&mut self, hwnd: HWND) {
        let mut rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rect) };
        let s = |v: i32| scale(hwnd, v);
        let width = (rect.right - 2 * s(draw::SPACE_XL)).max(0);
        let fonts = self.fonts.as_ref();
        let inputs = settings_page::Inputs {
            start_with_windows: self.settings.start_with_windows,
            history: self.settings.history,
            windows_search: windows_search_row(),
            sizes: sizes_text(self.sizes.as_ref()),
            drives: settings::indexed_drives(),
        };
        // Notes wrap inside their card: page width minus the card's side insets and
        // the text padding on both ends.
        let text_width = width - 2 * s(draw::SPACE_S) - 2 * s(draw::SPACE_L);
        let device = unsafe { GetDC(hwnd) };
        let note_height = |text: &str| {
            fonts.map_or(0, |fonts| {
                draw::wrapped_text_height(device, fonts.detail, text, text_width)
            })
        };
        self.settings_page = settings_page::build(width, s, note_height, &inputs);
        if !device.is_null() {
            unsafe { ReleaseDC(hwnd, device) };
        }
        let max_scroll = (self.settings_page.content - rect.bottom).max(0);
        self.settings_scroll = self.settings_scroll.min(max_scroll);
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    /// A switch was flipped: apply it at once, so there is nothing to save later.
    fn apply_toggle(&mut self, hwnd: HWND, kind: settings_page::Toggle) {
        let mut next = self.settings.clone();
        match kind {
            settings_page::Toggle::StartWithWindows => {
                next.start_with_windows = !next.start_with_windows
            }
            settings_page::Toggle::History => next.history = !next.history,
        }
        if next.start_with_windows != self.settings.start_with_windows
            && let Err(err) = next.apply_startup()
        {
            message(hwnd, &format!("Could not change start with Windows: {err}"));
            return;
        }
        if let Err(err) = next.save() {
            message(hwnd, &format!("Could not save settings: {err}"));
            return;
        }
        self.settings = next;
        self.rebuild_page(hwnd);
    }

    /// A captured combination replaces the hotkey, or the old one is put back and
    /// `false` returned with the band showing why.
    fn apply_captured_hotkey(&mut self, hwnd: HWND, modifiers: u32, key: u32) -> bool {
        if modifiers == self.settings.modifiers && key == self.settings.key {
            return true;
        }
        if self.hotkey_registered {
            unsafe { UnregisterHotKey(hwnd, HOTKEY_ID) };
        }
        if unsafe { RegisterHotKey(hwnd, HOTKEY_ID, modifiers, key) } == 0 {
            self.hotkey_registered = unsafe {
                RegisterHotKey(hwnd, HOTKEY_ID, self.settings.modifiers, self.settings.key)
            } != 0;
            self.capture_note = Some("That hotkey is already in use. Choose another.");
            return false;
        }
        self.hotkey_registered = true;
        self.settings.modifiers = modifiers;
        self.settings.key = key;
        if let Err(err) = self.settings.save() {
            message(hwnd, &format!("Could not save settings: {err}"));
        }
        true
    }

    /// What the hotkey band shows on its right: the prompt and keys held while
    /// capturing, otherwise the current combination.
    fn hotkey_display(&self) -> String {
        if self.capturing {
            if let Some(note) = self.capture_note {
                return note.to_string();
            }
            let modifiers = held_modifiers();
            if modifiers == 0 {
                "Press the keys…".to_string()
            } else {
                settings_page::hotkey_name(modifiers, 0)
            }
        } else {
            settings_page::hotkey_name(self.settings.modifiers, self.settings.key)
        }
    }

    /// A key while the band is being edited: modifiers only update the prompt; any
    /// other key either becomes the hotkey or explains why not.
    fn capture_key(&mut self, hwnd: HWND, vk: u16) {
        match vk {
            VK_ESCAPE => self.capturing = false,
            VK_CONTROL | VK_MENU | VK_SHIFT | VK_LWIN | VK_RWIN => {}
            _ => {
                let modifiers = held_modifiers();
                let key = vk as u32;
                if !settings_page::valid_combo(modifiers, key) {
                    self.capture_note = Some("Use Ctrl, Alt, Shift or Win with another key.");
                } else if self.apply_captured_hotkey(hwnd, modifiers, key) {
                    self.capturing = false;
                }
            }
        }
        unsafe { InvalidateRect(hwnd, null(), 0) };
    }

    /// The row the keyboard points at, stepped over headings and notes.
    fn step_cursor(&mut self, hwnd: HWND, down: bool) {
        if self.settings_page.lines.is_empty() {
            return;
        }
        let page = &self.settings_page;
        let last = page.lines.len() - 1;
        let mut index = self.settings_cursor.unwrap_or(0);
        loop {
            index = if down {
                if index >= last {
                    break;
                }
                index + 1
            } else if index == 0 {
                break;
            } else {
                index - 1
            };
            if !page.is_heading(index) {
                self.settings_cursor = Some(index);
                break;
            }
        }
        if let Some(index) = self.settings_cursor {
            // Keep the pointed-at row on screen: scroll just enough to see it.
            let mut rect = RECT::default();
            unsafe { GetClientRect(hwnd, &mut rect) };
            let row = page.lines[index].rect;
            let view = rect.bottom;
            if row.top - self.settings_scroll < 0 {
                self.settings_scroll = row.top;
            } else if row.bottom - self.settings_scroll > view {
                self.settings_scroll = (row.bottom - view).min(page.content - view).max(0);
            }
        }
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    /// Runs the row the keyboard or mouse chose.
    fn activate_row(&mut self, hwnd: HWND, index: usize) {
        let Some(line) = self.settings_page.lines.get(index) else {
            return;
        };
        match &line.row {
            settings_page::Row::Toggle { kind, .. } => {
                let kind = *kind;
                self.apply_toggle(hwnd, kind);
            }
            settings_page::Row::Action { kind, .. } => match kind {
                settings_page::Action::WindowsSearch => self.toggle_windows_search(hwnd),
                settings_page::Action::Updates => check_updates(hwnd),
                settings_page::Action::ClearHistory => match self.history.clear() {
                    Ok(()) => message(hwnd, "Open history cleared."),
                    Err(err) => message(hwnd, &format!("Could not clear the history: {err}")),
                },
            },
            _ => {}
        }
    }

    /// One click switches Windows search; the row's title says what the next press does.
    fn toggle_windows_search(&mut self, hwnd: HWND) {
        let off = !winsearch::is_off();
        let outcome = winsearch::request(hwnd, off);
        winkey::set_search_off(winsearch::is_off());
        if !outcome.changed {
            message(
                hwnd,
                "Windows search was not changed. Approve the administrator prompt to change it.",
            );
        } else if off && !outcome.index_cleared {
            message(
                hwnd,
                "Windows search is off, but its index files could not be removed, so its disk \
                 space was not freed. They are still in C:\\ProgramData\\Microsoft\\Search.",
            );
        } else if off {
            self.show_notice(hwnd, "Windows search is off, and its index files are gone");
        } else if !outcome.restored {
            message(
                hwnd,
                "Windows search is on again, but Windows would not put its background indexer \
                 back to starting automatically. If search does not work, set the \"Windows \
                 Search\" service to Automatic in Windows' Services app.",
            );
        } else {
            self.show_notice(hwnd, "Windows search is on again");
        }
        // Its footprint changed either way, so what the panel holds is now out of date.
        self.refresh_sizes(hwnd);
        self.rebuild_page(hwnd);
    }

    /// The first-run offer: one short explanation, then the user decides. Asked once only,
    /// and only after the first scan, so the numbers in it are measured rather than claimed.
    fn offer_windows_search(&mut self, hwnd: HWND) {
        if self.settings.windows_search_asked {
            return;
        }
        if winsearch::is_off() {
            self.remember_windows_search(hwnd);
            return;
        }
        // The offer waits for the panel to be on screen and idle: a dialog that
        // appears over a search the user is typing would be in the way. A busy first
        // open is asked again when the sizes reading for Settings arrives.
        let idle = self.last_text.trim().is_empty();
        if idle && unsafe { IsWindowVisible(hwnd) } != 0 {
            self.show_windows_search_offer(hwnd);
        } else {
            self.offer_pending = true;
        }
    }

    fn show_windows_search_offer(&mut self, hwnd: HWND) {
        self.offer_pending = false;
        // "No" is the default answer: turning search off is a machine-wide change.
        let answer = unsafe {
            MessageBoxW(
                hwnd,
                wide(offer_text()).as_ptr(),
                wide(OFFER_TITLE).as_ptr(),
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
            )
        };
        self.remember_windows_search(hwnd);
        if answer == IDYES {
            self.toggle_windows_search(hwnd);
        }
    }

    /// Starts the one sizes request the panel ever needs.
    fn start_sizes(&mut self, hwnd: HWND) {
        if self.sizes_results.is_some() {
            return;
        }
        let (results, stop) = stats::start(hwnd);
        self.sizes_results = Some(results);
        self.sizes_stop = Some(stop);
    }

    /// Asks again from scratch, after something changed what the numbers describe.
    fn refresh_sizes(&mut self, hwnd: HWND) {
        self.sizes = None;
        self.sizes_results = None;
        self.sizes_stop = None;
        self.start_sizes(hwnd);
    }

    /// A sizes reading arrived. It fills in the Settings page's footprint note, and
    /// shows an offer that was postponed because the panel was busy.
    fn apply_sizes(&mut self, hwnd: HWND, reading: stats::Reading) {
        self.sizes = Some(reading);
        if self.settings_open {
            self.rebuild_page(hwnd);
        }
        let idle = self.last_text.trim().is_empty();
        if self.offer_pending && idle && unsafe { IsWindowVisible(hwnd) } != 0 {
            self.show_windows_search_offer(hwnd);
        }
    }

    fn remember_windows_search(&mut self, hwnd: HWND) {
        self.settings.windows_search_asked = true;
        if let Err(err) = self.settings.save() {
            message(hwnd, &format!("Could not save settings: {err}"));
        }
    }

    fn layout(&mut self, hwnd: HWND) {
        let mut rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rect) };
        let pad = scale(hwnd, draw::SPACE_XL);
        let width = (rect.right - 2 * pad).max(0);
        if self.settings_open {
            // The page draws itself from the client rect; only the native hotkey
            // control is placed, which `rebuild_page` does.
            self.rebuild_page(hwnd);
            return;
        }
        let s = |v: i32| scale(hwnd, v);
        // Search field, results, footer: three bands on the spacing scale.
        self.field = RECT {
            left: pad,
            top: pad,
            right: pad + width,
            bottom: pad + s(40),
        };
        let edit_height = s(24);
        let edit_left = self.field.left + s(40);
        // The settings button sits at the right end of the field.
        let gear = s(36);
        self.gear = RECT {
            left: self.field.right - gear,
            top: self.field.top,
            right: self.field.right,
            bottom: self.field.bottom,
        };
        let footer_height = s(28);
        self.footer = RECT {
            left: pad,
            top: rect.bottom - s(draw::SPACE_S) - footer_height,
            right: pad + width,
            bottom: rect.bottom - s(draw::SPACE_S),
        };
        // Row highlights are inset by SPACE_S, so the list overhangs the field by that
        // much and highlights line up with the field's edges.
        let list_left = pad - s(draw::SPACE_S);
        let list_top = self.field.bottom + s(draw::SPACE_M);
        let list_height = (self.footer.top - s(draw::SPACE_S) - list_top).max(0);
        unsafe {
            MoveWindow(
                self.edit,
                edit_left,
                self.field.top + (s(40) - edit_height) / 2,
                (self.gear.left - s(draw::SPACE_S) - edit_left).max(0),
                edit_height,
                1,
            );
            SendMessageW(self.edit, EM_SETMARGINS, EC_LEFTMARGIN | EC_RIGHTMARGIN, 0);
            MoveWindow(
                self.list,
                list_left,
                list_top,
                width + 2 * s(draw::SPACE_S),
                list_height,
                1,
            );
            InvalidateRect(hwnd, null(), 1);
        }
        self.refresh_status();
        self.fit_column();
    }

    fn hints_width(&self, hwnd: HWND) -> i32 {
        let Some(fonts) = &self.fonts else { return 0 };
        let dc = unsafe { GetDC(hwnd) };
        let hints: Vec<u16> = KEY_HINTS.encode_utf16().collect();
        let width = draw::text_width(dc, fonts.detail, &hints);
        unsafe { ReleaseDC(hwnd, dc) };
        width
    }

    /// Rebuilds the fonts for the current DPI and hands them to every child.
    fn apply_font(&mut self, hwnd: HWND) {
        if let Some(old) = self.fonts.take() {
            old.delete();
        }
        let fonts = draw::Fonts::new(hwnd);
        let assign = |control: HWND, font: HFONT| {
            if !control.is_null() {
                unsafe { SendMessageW(control, WM_SETFONT, font as usize, 1) };
            }
        };
        assign(self.edit, fonts.search);
        assign(self.status_label, fonts.detail);
        assign(self.list, fonts.name);
        self.fonts = Some(fonts);
        // An owner-drawn list asks for its row height only when it is placed, so a
        // made-up position change makes it ask again after a DPI change.
        let mut rect = RECT::default();
        unsafe { GetWindowRect(self.list, &mut rect) };
        let position = WINDOWPOS {
            hwnd: self.list,
            cx: rect.right - rect.left,
            cy: rect.bottom - rect.top,
            flags: SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            ..Default::default()
        };
        unsafe {
            SendMessageW(
                self.list,
                WM_WINDOWPOSCHANGED,
                0,
                (&raw const position) as isize,
            )
        };
    }

    /// The footer's left side: a brief notice, or the search state.
    /// The key hint on the right gives way when the status needs the room.
    fn refresh_status(&self) {
        if self.status_label.is_null() {
            return;
        }
        let text = self.notice.as_deref().unwrap_or(&self.status);
        let main =
            unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetParent(self.status_label) };
        let footer = self.footer;
        let width = footer.right - footer.left;
        let gap = scale(main, draw::SPACE_L);
        let hints = self.hints_width(main);
        let needed = self.fonts.as_ref().map_or(0, |fonts| {
            let dc = unsafe { GetDC(main) };
            let value: Vec<u16> = text.encode_utf16().collect();
            let width = draw::text_width(dc, fonts.detail, &value);
            unsafe { ReleaseDC(main, dc) };
            width
        });
        let show = needed + gap + hints <= width;
        self.show_hints.set(show);
        let label_width = if show { width - hints - gap } else { width };
        unsafe {
            MoveWindow(
                self.status_label,
                footer.left,
                footer.top,
                label_width.max(0),
                footer.bottom - footer.top,
                1,
            );
            windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                self.status_label,
                wide(text).as_ptr(),
            );
            InvalidateRect(main, &footer, 1);
        }
    }

    fn theme(&mut self, hwnd: HWND) {
        let light = settings::light_theme();
        let colors = draw::palette(light, settings::accent_color());
        if self.light == Some(light) && self.colors.accent == colors.accent {
            return;
        }
        self.light = Some(light);
        self.colors = colors;
        if !self.background.is_null() {
            unsafe { DeleteObject(self.background) };
        }
        if !self.surface_brush.is_null() {
            unsafe { DeleteObject(self.surface_brush) };
        }
        self.background = unsafe { CreateSolidBrush(colors.panel) };
        self.surface_brush = unsafe { CreateSolidBrush(colors.surface) };
        let dark: i32 = (!light).into();
        let corner: i32 = DWMWCP_ROUND;
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                (&raw const dark).cast(),
                size_of::<i32>() as u32,
            );
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                (&raw const corner).cast(),
                size_of::<i32>() as u32,
            );
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR as u32,
                (&raw const colors.edge).cast(),
                size_of::<u32>() as u32,
            );
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_COLOR as u32,
                (&raw const colors.panel).cast(),
                size_of::<u32>() as u32,
            );
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TEXT_COLOR as u32,
                (&raw const colors.text).cast(),
                size_of::<u32>() as u32,
            );
        }
        let theme = wide(if light {
            "Explorer"
        } else {
            "DarkMode_Explorer"
        });
        unsafe {
            SetWindowTheme(self.list, theme.as_ptr(), null());
            SendMessageW(
                self.list,
                windows_sys::Win32::UI::Controls::LVM_SETBKCOLOR,
                0,
                colors.panel as isize,
            );
            SendMessageW(
                self.list,
                windows_sys::Win32::UI::Controls::LVM_SETTEXTBKCOLOR,
                0,
                colors.panel as isize,
            );
            SendMessageW(
                self.list,
                windows_sys::Win32::UI::Controls::LVM_SETTEXTCOLOR,
                0,
                colors.text as isize,
            );
            InvalidateRect(hwnd, null(), 1);
            InvalidateRect(self.list, null(), 1);
        }
    }
}

fn message(hwnd: HWND, text: &str) {
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            hwnd,
            wide(text).as_ptr(),
            wide("better_search").as_ptr(),
            0,
        )
    };
}

/// A yes/no question in the same style as [`message`]; true when the answer is Yes.
fn confirm(hwnd: HWND, text: &str) -> bool {
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            hwnd,
            wide(text).as_ptr(),
            wide("better_search").as_ptr(),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

/// Asks the release host for the newest version and, if the user agrees, installs it.
/// The network work runs on its own thread, because a slow answer must not freeze the
/// panel; every answer comes back as a message box.
fn check_updates(hwnd: HWND) {
    let owner = hwnd as usize;
    let started = std::thread::Builder::new()
        .name("update".into())
        .spawn(move || {
            let hwnd = owner as HWND;
            let manifest = match update::check() {
                Ok(Some(manifest)) => manifest,
                Ok(None) => {
                    message(
                        hwnd,
                        &format!(
                            "You have the newest version, better_search {}.",
                            update::current()
                        ),
                    );
                    return;
                }
                Err(err) => {
                    message(hwnd, &format!("Could not check for updates.\n\n{err}"));
                    return;
                }
            };
            let question = format!(
                "better_search {} is ready; you have {}.\n\nDownload and install it now?\n\nWindows will ask for permission, then the new version starts.",
                manifest.version,
                update::current()
            );
            if !confirm(hwnd, &question) {
                return;
            }
            let installer = match update::download(&manifest) {
                Ok(installer) => installer,
                Err(err) => {
                    message(hwnd, &err);
                    return;
                }
            };
            match update::install(hwnd, &installer) {
                Ok(()) if confirm(hwnd, &format!("Restart better_search {} now?", manifest.version)) => {
                    unsafe { PostMessageW(hwnd, WM_RESTART, 0, 0) };
                }
                Ok(()) => message(
                    hwnd,
                    &format!(
                        "better_search {} starts the next time better_search runs.",
                        manifest.version
                    ),
                ),
                Err(err) => message(hwnd, &err),
            }
        });
    if started.is_err() {
        message(hwnd, "Could not start the update check.");
    }
}

/// Closes this window and starts the freshly installed program in its place. The
/// installer moved the old program aside, so the path this one was started from now
/// holds the new build.
fn restart(hwnd: HWND) {
    let Ok(program) = std::env::current_exe() else {
        return;
    };
    // Start the new program the way this one started: hidden if the panel is hidden.
    let visible = unsafe { IsWindowVisible(hwnd) } != 0;
    unsafe { DestroyWindow(hwnd) };
    let mut command = std::process::Command::new(program);
    if !visible {
        command.arg("--hidden");
    }
    let _ = command.spawn();
}

fn control(hwnd: HWND, class: &[u16], title: &str, id: usize, style: u32) -> HWND {
    let child = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            wide(title).as_ptr(),
            WS_CHILD | WS_TABSTOP | style,
            0,
            0,
            0,
            0,
            hwnd,
            id as _,
            null_mut(),
            null(),
        )
    };
    unsafe { SendMessageW(child, WM_SETFONT, GetStockObject(DEFAULT_GUI_FONT) as _, 1) };
    child
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_MESSAGE,
        hIcon: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
        ..Default::default()
    };
    let label = wide("better_search");
    data.szTip[..label.len()].copy_from_slice(&label);
    data
}

fn popup(hwnd: HWND, items: &[(usize, &str)]) {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }
    for &(id, title) in items {
        let title = wide(title);
        unsafe { AppendMenuW(menu, MF_STRING, id, title.as_ptr()) };
    }
    let mut point = POINT::default();
    unsafe {
        GetCursorPos(&mut point);
        SetForegroundWindow(hwnd);
        TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, 0, hwnd, null());
        DestroyMenu(menu);
    }
}

unsafe extern "system" fn child_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    parent: usize,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        unsafe { RemoveWindowSubclass(hwnd, Some(child_proc), id) };
    }
    let main = parent as HWND;
    if (msg == WM_SETFOCUS || msg == WM_KILLFOCUS) && id == EDIT_ID {
        // The field's accent underline follows the focus.
        unsafe { InvalidateRect(main, null(), 0) };
    }
    let ptr = unsafe { GetWindowLongPtrW(main, GWLP_USERDATA) as *mut App };
    if id == LIST_ID && !ptr.is_null() {
        let app = unsafe { &mut *ptr };
        let (x, y) = (
            (l & 0xffff) as i16 as i32,
            ((l >> 16) & 0xffff) as i16 as i32,
        );
        match msg {
            WM_MOUSEMOVE => {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe { TrackMouseEvent(&mut track) };
                let row = app
                    .row_at(x, y)
                    .filter(|&row| matches!(app.rows[row], rows::Row::Hit(_)));
                app.set_hover(row);
            }
            WM_MOUSELEAVE => app.set_hover(None),
            WM_LBUTTONDOWN if app.click(main, x, y, true) => return 0,
            // The second press of a double-click on a button must not act twice, and
            // must not open the row.
            WM_LBUTTONDBLCLK if app.click(main, x, y, false) => return 0,
            WM_RBUTTONDOWN
                if app
                    .row_at(x, y)
                    .is_some_and(|row| matches!(app.rows[row], rows::Row::Section(_))) =>
            {
                return 0;
            }
            _ => {}
        }
    }
    if msg == WM_KEYDOWN && !ptr.is_null() {
        let app = unsafe { &mut *ptr };
        match w as u16 {
            VK_ESCAPE => {
                app.hide_panel(main);
                return 0;
            }
            VK_RETURN => {
                app.open_selected(main, unsafe { GetKeyState(VK_CONTROL as i32) } < 0);
                return 0;
            }
            VK_TAB => {
                unsafe { SetFocus(if id == EDIT_ID { app.list } else { app.edit }) };
                return 0;
            }
            0x48 if unsafe { GetKeyState(VK_CONTROL as i32) } < 0 => {
                app.include_system = !app.include_system;
                app.query(main);
                return 0;
            }
            VK_DOWN | VK_UP => {
                app.step_selection(w as u16 == VK_DOWN);
                return 0;
            }
            _ => {}
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, w, l) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// The modifiers held right now, as `RegisterHotKey`'s flags.
fn held_modifiers() -> u32 {
    let held = |vk: i32| unsafe { GetKeyState(vk) } < 0;
    (u32::from(held(VK_CONTROL as i32)) * 0x2)
        | (u32::from(held(VK_SHIFT as i32)) * 0x4)
        | u32::from(held(VK_MENU as i32))
        | (u32::from(held(VK_LWIN as i32) || held(VK_RWIN as i32)) * 0x8)
}

/// Extensions of shortcuts, shown without it: `Excel` instead of `Excel.lnk`.
const SHORTCUT_EXTENSIONS: &[&str] = &["lnk", "url", "appref-ms"];
const PROGRAM_EXTENSIONS: &[&str] = &["exe", "msc", "cpl", "bat", "cmd", "com"];

fn extension(hit: &Hit) -> Option<String> {
    if hit.path.starts_with(apps::PREFIX) {
        return None;
    }
    let name = hit.path.rsplit('\\').next()?;
    (!hit.is_dir)
        .then(|| {
            name.rsplit_once('.')
                .map(|(_, ext)| ext.to_ascii_lowercase())
        })
        .flatten()
}

/// Shortcuts and programs; web links are not apps.
fn is_app(hit: &Hit) -> bool {
    hit.path.starts_with(apps::PREFIX)
        || extension(hit).is_some_and(|ext| {
            ext != "url"
                && (SHORTCUT_EXTENSIONS.contains(&ext.as_str())
                    || PROGRAM_EXTENSIONS.contains(&ext.as_str()))
        })
}

fn display_name(hit: &Hit) -> String {
    let path = hit.path.trim_end_matches('\\');
    let name = path.rsplit('\\').next().unwrap_or(path);
    match (extension(hit), name.rsplit_once('.')) {
        (Some(ext), Some((stem, _))) if SHORTCUT_EXTENSIONS.contains(&ext.as_str()) => stem.into(),
        _ => name.into(),
    }
}

/// What the one-click Windows-search button in Settings will do next.
fn windows_search_label() -> &'static str {
    if winsearch::is_off() {
        "Turn Windows search back on (asks for admin)"
    } else {
        "Turn Windows search off (asks for admin)"
    }
}

/// The Windows-search row's title and second line, from the current state.
fn windows_search_row() -> (String, String) {
    let title = windows_search_label().to_owned();
    let detail = if winsearch::is_off() {
        "Windows search is off; its index files were removed".into()
    } else {
        "Windows search is on, and indexes in the background".into()
    };
    (title, detail)
}

/// The folder holding `path`, or the path itself for a drive root.
fn parent_folder(path: &str) -> &str {
    let path = path.trim_end_matches('\\');
    path.rsplit_once('\\').map_or(path, |(parent, _)| parent)
}

/// The letter part (`C:`) of a drive-root path (`C:` or `C:\`); None for anything else.
fn drive_letter(path: &str) -> Option<&str> {
    match path.as_bytes() {
        [c, b':'] | [c, b':', b'\\'] if c.is_ascii_alphabetic() => Some(&path[..2]),
        _ => None,
    }
}

/// What the drive holds, from Windows' own idea of it (`GetDriveTypeW`).
fn drive_kind(letter: &str) -> &'static str {
    let root: Vec<u16> = format!("{letter}\\").encode_utf16().chain([0]).collect();
    match unsafe { GetDriveTypeW(root.as_ptr()) } {
        2 => "Removable drive",
        3 => "Local disk",
        4 => "Network drive",
        5 => "Disc drive",
        _ => "Drive",
    }
}

/// The row name of a drive: the volume's label if it has one, with the letter.
fn drive_row_name(letter: &str) -> String {
    let root: Vec<u16> = format!("{letter}\\").encode_utf16().chain([0]).collect();
    let mut label = [0u16; 256];
    let named = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            0,
        )
    } != 0;
    let end = label.iter().position(|&c| c == 0).unwrap_or(0);
    match named && end > 0 {
        true => format!("{} ({letter})", String::from_utf16_lossy(&label[..end])),
        false => format!("{} ({letter})", drive_kind(letter)),
    }
}

/// The second line: what an app or link is, in words, or the folder a file is in.
fn describe(hit: &Hit) -> String {
    if let Some(letter) = drive_letter(&hit.path) {
        return drive_kind(letter).into();
    }
    if hit.path.starts_with(apps::PREFIX) {
        return "App".into();
    }
    if commands::is_power(&hit.path) {
        return "Power command".into();
    }
    if commands::is_command(&hit.path) {
        return "Windows Settings".into();
    }
    let folder = parent_folder(&hit.path);
    let Some(ext) = extension(hit) else {
        return folder.into();
    };
    let lower = hit.path.to_lowercase();
    let place = if lower
        .rsplit_once('\\')
        .is_some_and(|(parent, _)| parent.ends_with("\\desktop"))
    {
        "on the Desktop".to_string()
    } else {
        format!("in {folder}")
    };
    let in_windows = lower
        .get(1..)
        .is_some_and(|rest| rest.starts_with(":\\windows\\"));
    let program = PROGRAM_EXTENSIONS.contains(&ext.as_str());
    match ext.as_str() {
        "lnk" | "appref-ms" if lower.contains("\\start menu\\programs\\") => "App".into(),
        "lnk" | "appref-ms" => format!("App shortcut {place}"),
        "url" => format!("Web link {place}"),
        _ if program && in_windows => "Windows tool".into(),
        _ if program => format!("App in {folder}"),
        _ => folder.into(),
    }
}

const DOCUMENT_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "docm", "odt", "rtf", "txt", "md", "xls", "xlsx", "xlsm", "ods", "csv",
    "ppt", "pptx", "odp", "one", "vsdx", "pub", "msg", "eml", "epub", "mobi",
];
const MEDIA_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "avif", "tif", "tiff", "svg", "psd", "dng",
    "cr2", "cr3", "nef", "arw", "mp4", "m4v", "mkv", "mov", "avi", "webm", "wmv", "mp3", "wav",
    "flac", "m4a", "aac", "ogg", "wma", "opus",
];

/// The section a hit is listed under.
fn kind_of(hit: &Hit) -> rows::Kind {
    if commands::is_command(&hit.path) {
        return rows::Kind::Settings;
    }
    if is_app(hit) {
        return rows::Kind::Apps;
    }
    if hit.is_dir {
        return rows::Kind::Folders;
    }
    match extension(hit) {
        Some(ext) if DOCUMENT_EXTENSIONS.contains(&ext.as_str()) => rows::Kind::Documents,
        Some(ext) if MEDIA_EXTENSIONS.contains(&ext.as_str()) => rows::Kind::Media,
        _ => rows::Kind::Files,
    }
}

fn plural(count: u32, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// The footer's summary of a search: results shown, and how many Ctrl+H would add.
fn status_text(shown: u32, hidden: u32) -> String {
    match (shown, hidden) {
        (0, 0) => "No results".into(),
        (0, _) => format!("No results · {hidden} more with Ctrl+H"),
        (_, 0) => plural(shown, "result", "results"),
        _ => format!(
            "{} · {hidden} more with Ctrl+H",
            plural(shown, "result", "results")
        ),
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let app = Box::into_raw(Box::new(App::new()));
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize) };
        return 1;
    }
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    }
    if msg == WM_NCDESTROY {
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let app = Box::from_raw(ptr);
            for brush in [app.background, app.surface_brush] {
                if !brush.is_null() {
                    DeleteObject(brush);
                }
            }
            if let Some(fonts) = &app.fonts {
                fonts.delete();
            }
            drop(app);
        }
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    }
    let app = unsafe { &mut *ptr };
    if msg != 0 && msg == app.taskbar_message {
        unsafe { Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd)) };
        return 0;
    }
    match msg {
        WM_CREATE => {
            let edit_class = wide("EDIT");
            let hint = wide("");
            app.edit = unsafe {
                CreateWindowExW(
                    0,
                    edit_class.as_ptr(),
                    hint.as_ptr(),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                    0,
                    0,
                    0,
                    0,
                    hwnd,
                    EDIT_ID as _,
                    null_mut(),
                    null(),
                )
            };
            app.list = unsafe {
                CreateWindowExW(
                    0,
                    WC_LISTVIEWW,
                    null(),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | LVS_REPORT
                        | LVS_OWNERDATA
                        | LVS_OWNERDRAWFIXED
                        | LVS_NOCOLUMNHEADER
                        | LVS_SINGLESEL
                        | LVS_SHOWSELALWAYS
                        | LVS_SHAREIMAGELISTS,
                    0,
                    0,
                    0,
                    0,
                    hwnd,
                    LIST_ID as _,
                    null_mut(),
                    null(),
                )
            };
            app.status_label = control(
                hwnd,
                &wide("STATIC"),
                "",
                STATUS_ID,
                WS_VISIBLE | SS_CENTERIMAGE | SS_ENDELLIPSIS,
            );
            let cue = wide("Search apps, files and folders");
            // wParam 1: keep the placeholder while the empty box has focus.
            unsafe { SendMessageW(app.edit, EM_SETCUEBANNER, 1, cue.as_ptr() as isize) };
            unsafe {
                SetWindowSubclass(app.edit, Some(child_proc), EDIT_ID, hwnd as usize);
                SetWindowSubclass(app.list, Some(child_proc), LIST_ID, hwnd as usize);
            }
            app.apply_font(hwnd);
            unsafe {
                SendMessageW(
                    app.list,
                    LVM_SETEXTENDEDLISTVIEWSTYLE,
                    0,
                    (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as _,
                );
            }
            let label = wide("Result");
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: scale(hwnd, 400),
                pszText: label.as_ptr() as _,
                ..Default::default()
            };
            unsafe {
                SendMessageW(
                    app.list,
                    LVM_INSERTCOLUMNW,
                    0,
                    (&col as *const LVCOLUMNW) as _,
                )
            };
            let (sender, results) = search::start(hwnd);
            app.sender = Some(sender);
            app.results = Some(results);
            let (sender, results) = thumbs::start(hwnd, Arc::clone(&app.thumb_generation));
            app.thumb_sender = Some(sender);
            app.thumb_results = Some(results);
            app.apps.refresh_if_stale();
            app.theme(hwnd);
            app.register_hotkey(hwnd);
            winkey::install(winsearch::is_off());
            unsafe { Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd)) };
            app.update_title(hwnd);
            0
        }
        WM_SIZE => {
            app.layout(hwnd);
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED => {
            app.theme(hwnd);
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_LBUTTONDOWN => {
            let x = (l & 0xffff) as i16 as i32;
            let y = ((l >> 16) & 0xffff) as i16 as i32;
            if app.settings_open {
                // The page takes the mouse back; a row click acts at once.
                unsafe { SetFocus(hwnd) };
                let band = app.settings_page.hotkey;
                let in_band = x >= band.left
                    && x < band.right
                    && y + app.settings_scroll >= band.top
                    && y + app.settings_scroll < band.bottom;
                if in_band {
                    // The band is the field now: the next keys become the hotkey.
                    app.capturing = true;
                    app.capture_note = None;
                    unsafe { InvalidateRect(hwnd, null(), 0) };
                    return 0;
                }
                app.capturing = false;
                app.capture_note = None;
                let index = app
                    .settings_page
                    .line_at(x, y + app.settings_scroll)
                    .filter(|&index| !app.settings_page.is_heading(index));
                app.settings_cursor = index;
                if let Some(index) = index {
                    app.activate_row(hwnd, index);
                }
                return 0;
            }
            let gear = app.gear;
            if !app.settings_open
                && gear.right > gear.left
                && x >= gear.left
                && x < gear.right
                && y >= gear.top
                && y < gear.bottom
            {
                app.show_settings(hwnd);
                return 0;
            }
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_MOUSEMOVE if app.settings_open => {
            let x = (l & 0xffff) as i16 as i32;
            let y = ((l >> 16) & 0xffff) as i16 as i32;
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe { TrackMouseEvent(&mut track) };
            let index = app
                .settings_page
                .line_at(x, y + app.settings_scroll)
                .filter(|&index| !app.settings_page.is_heading(index));
            if app.settings_hover != index {
                app.settings_hover = index;
                unsafe { InvalidateRect(hwnd, null(), 1) };
            }
            0
        }
        WM_MOUSELEAVE if app.settings_open => {
            if app.settings_hover.take().is_some() {
                unsafe { InvalidateRect(hwnd, null(), 1) };
            }
            0
        }
        WM_MOUSEWHEEL if app.settings_open => {
            // The page scrolls when it is taller than the window.
            let lines = ((w >> 16) as i16 as i32 / 120) * 3;
            let mut rect = RECT::default();
            unsafe { GetClientRect(hwnd, &mut rect) };
            let row = scale(hwnd, draw::ROW_HEIGHT);
            let max = (app.settings_page.content - rect.bottom).max(0);
            let next = (app.settings_scroll - lines * row).clamp(0, max);
            if next != app.settings_scroll {
                app.settings_scroll = next;
                unsafe { InvalidateRect(hwnd, null(), 1) };
            }
            0
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let device = unsafe { BeginPaint(hwnd, &mut paint) };
            let mut rect = RECT::default();
            unsafe { GetClientRect(hwnd, &mut rect) };
            unsafe { FillRect(device, &rect, app.background) };
            if let Some(fonts) = &app.fonts
                && app.settings_open
            {
                let view = settings_page::View {
                    hover: app.settings_hover,
                    cursor: app.settings_cursor,
                    hotkey_text: app.hotkey_display(),
                    capturing: app.capturing,
                    offset: app.settings_scroll,
                };
                settings_page::paint(
                    device,
                    &app.settings_page,
                    fonts,
                    &app.colors,
                    &view,
                    &|v| scale(hwnd, v),
                );
            } else if let Some(fonts) = &app.fonts
                && app.field.right > app.field.left
            {
                let s = |v: i32| scale(hwnd, v);
                let colors = app.colors;
                let field = app.field;
                let radius = s(draw::RADIUS);
                draw::fill_round(device, &field, radius, colors.edge);
                let inner = RECT {
                    left: field.left + 1,
                    top: field.top + 1,
                    right: field.right - 1,
                    bottom: field.bottom - 1,
                };
                draw::fill_round(device, &inner, radius, colors.surface);
                if unsafe { GetFocus() } == app.edit {
                    // Windows 11 text boxes mark focus with an accent underline.
                    let line = RECT {
                        left: field.left + radius / 2,
                        top: field.bottom - s(2),
                        right: field.right - radius / 2,
                        bottom: field.bottom,
                    };
                    let brush = unsafe { CreateSolidBrush(colors.accent) };
                    unsafe {
                        FillRect(device, &line, brush);
                        DeleteObject(brush);
                    }
                }
                let glyph = RECT {
                    right: field.left + s(40),
                    ..field
                };
                draw::text(
                    device,
                    draw::GLYPH_SEARCH,
                    &glyph,
                    fonts.glyph,
                    colors.secondary,
                    DT_CENTER,
                );
                if app.gear.right > app.gear.left {
                    draw::text(
                        device,
                        draw::GLYPH_SETTINGS,
                        &app.gear,
                        fonts.glyph,
                        colors.secondary,
                        DT_CENTER,
                    );
                }
                let footer = app.footer;
                let divider = RECT {
                    top: footer.top - s(draw::SPACE_S),
                    bottom: footer.top - s(draw::SPACE_S) + 1,
                    ..footer
                };
                let brush = unsafe { CreateSolidBrush(colors.edge) };
                unsafe {
                    FillRect(device, &divider, brush);
                    DeleteObject(brush);
                }
                if app.show_hints.get() {
                    draw::text(
                        device,
                        KEY_HINTS,
                        &footer,
                        fonts.detail,
                        colors.secondary,
                        DT_RIGHT,
                    );
                }
            }
            unsafe { EndPaint(hwnd, &paint) };
            0
        }
        WM_ERASEBKGND => {
            let mut rect = RECT::default();
            unsafe {
                GetClientRect(hwnd, &mut rect);
                FillRect(w as _, &rect, app.background)
            };
            1
        }
        WM_CTLCOLOREDIT if l as HWND == app.edit => {
            unsafe {
                SetBkColor(w as _, app.colors.surface);
                SetTextColor(w as _, app.colors.text);
            }
            app.surface_brush as _
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let text = if l as HWND == app.status_label {
                app.colors.secondary
            } else {
                app.colors.text
            };
            unsafe {
                SetBkColor(w as _, app.colors.panel);
                SetTextColor(w as _, text);
            }
            app.background as _
        }
        WM_MEASUREITEM => {
            let item = unsafe { &mut *(l as *mut MEASUREITEMSTRUCT) };
            if item.CtlID as usize == LIST_ID {
                item.itemHeight = scale(hwnd, draw::ROW_HEIGHT) as u32;
                return 1;
            }
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(l as *const DRAWITEMSTRUCT) };
            if item.CtlID as usize == LIST_ID {
                app.draw_row(item);
                return 1;
            }
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_TIMER if w == NOTICE_TIMER => {
            unsafe { KillTimer(hwnd, NOTICE_TIMER) };
            app.notice = None;
            app.refresh_status();
            0
        }
        WM_COMMAND if w & 0xffff == EDIT_ID && (w >> 16) as u32 == EN_CHANGE => {
            app.query(hwnd);
            0
        }
        WM_COMMAND => {
            match w & 0xffff {
                MENU_OPEN => app.open_panel(hwnd),
                MENU_SETTINGS => app.show_settings(hwnd),
                MENU_PAUSE => {
                    app.paused = !app.paused;
                    app.query(hwnd);
                }
                MENU_QUIT => {
                    unsafe { DestroyWindow(hwnd) };
                }
                MENU_RESULT_OPEN => app.open_selected(hwnd, false),
                MENU_RESULT_FOLDER => app.open_selected(hwnd, true),
                MENU_RESULT_COPY => {
                    app.copy_selected(hwnd);
                    app.show_notice(hwnd, "Path copied");
                }
                MENU_UPDATE => check_updates(hwnd),
                _ => {}
            }
            0
        }
        TRAY_MESSAGE => {
            match l as u32 {
                WM_LBUTTONDBLCLK => app.open_panel(hwnd),
                WM_RBUTTONUP => popup(
                    hwnd,
                    &[
                        (MENU_OPEN, "Open search"),
                        (MENU_SETTINGS, "Settings"),
                        (MENU_PAUSE, if app.paused { "Resume" } else { "Pause" }),
                        (MENU_UPDATE, "Check for updates"),
                        (MENU_QUIT, "Quit"),
                    ],
                ),
                _ => {}
            }
            0
        }
        WM_HOTKEY if w as i32 == HOTKEY_ID => {
            app.open_panel(hwnd);
            0
        }
        WM_SHOW_PANEL => {
            app.open_panel(hwnd);
            0
        }
        WM_RESTART => {
            restart(hwnd);
            0
        }
        WM_QUIT_PANEL => {
            // The tray's Quit does the same: the panel has nothing to save.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_TIMER if w == RETRY_TIMER => {
            unsafe { KillTimer(hwnd, RETRY_TIMER) };
            if !app.paused
                && !app.last_text.trim().is_empty()
                && unsafe { IsWindowVisible(hwnd) } != 0
            {
                app.serial = app.serial.wrapping_add(1);
                if let Some(sender) = &app.sender {
                    let _ = sender.send(search::Request {
                        serial: app.serial,
                        text: app.last_text.clone(),
                        include_system: app.include_system,
                    });
                }
            }
            0
        }
        WM_DPICHANGED => {
            let rect = unsafe { &*(l as *const RECT) };
            app.apply_font(hwnd);
            app.layout(hwnd);
            unsafe {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER,
                );
            }
            0
        }
        thumbs::WM_THUMBNAIL => {
            app.thumbnails_ready();
            0
        }
        search::WM_SEARCH_RESULT => {
            if let Some(results) = &app.results {
                let pending: Vec<_> = results.try_iter().collect();
                for result in pending {
                    app.apply_result(hwnd, result);
                }
            }
            0
        }
        stats::WM_SIZES_RESULT => {
            if let Some(results) = &app.sizes_results {
                let pending: Vec<_> = results.try_iter().collect();
                for reading in pending {
                    app.apply_sizes(hwnd, reading);
                }
            }
            0
        }
        WM_NOTIFY => {
            let hdr = unsafe { &*(l as *const NMHDR) };
            if hdr.hwndFrom == app.list {
                if hdr.code == LVN_GETDISPINFOW {
                    // Rows are drawn by `draw_row`; the text here is what screen
                    // readers announce.
                    let disp = unsafe { &mut *(l as *mut NMLVDISPINFOW) };
                    let text = match app.rows.get(disp.item.iItem as usize) {
                        Some(rows::Row::Section(kind)) => kind.label().to_string(),
                        Some(&rows::Row::Hit(index)) => format!(
                            "{}, {}",
                            String::from_utf16_lossy(&app.names[index]),
                            app.details[index]
                        ),
                        None => String::new(),
                    };
                    if disp.item.mask & LVIF_TEXT != 0
                        && !disp.item.pszText.is_null()
                        && disp.item.cchTextMax > 0
                    {
                        let text = wide(&text);
                        let count = text.len().min(disp.item.cchTextMax as usize);
                        unsafe {
                            std::ptr::copy_nonoverlapping(text.as_ptr(), disp.item.pszText, count);
                            *disp.item.pszText.add(count - 1) = 0;
                        }
                    }
                    return 0;
                }
                if hdr.code == LVN_ITEMCHANGED {
                    // Headings cannot be selected (Home, Page Up and clicks land on them).
                    if let Some(row) = app.selected_row()
                        && matches!(app.rows[row], rows::Row::Section(_))
                        && let Some(&rows::Row::Hit(index)) = app.rows.get(row + 1)
                    {
                        app.select(index);
                    }
                    return 0;
                }
                if hdr.code == NM_DBLCLK {
                    app.open_selected(hwnd, false);
                    return 0;
                }
                if hdr.code == windows_sys::Win32::UI::Controls::NM_RCLICK {
                    if app.selected().is_some() {
                        popup(
                            hwnd,
                            &[
                                (MENU_RESULT_OPEN, "Open"),
                                (MENU_RESULT_FOLDER, "Show in folder"),
                                (MENU_RESULT_COPY, "Copy path"),
                            ],
                        );
                    }
                    return 0;
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_KEYDOWN | WM_SYSKEYDOWN if app.settings_open && app.capturing => {
            // With Alt held the keys arrive as WM_SYSKEYDOWN; swallow both, or the
            // system menu would open instead of the capture finishing.
            app.capture_key(hwnd, w as u16);
            0
        }
        WM_SYSCHAR if app.settings_open => {
            // TranslateMessage turns Alt keys into this; letting it through opens
            // the window's system menu, which has no place on the page.
            0
        }
        WM_SYSCOMMAND if app.settings_open && (w & 0xfff0) == SC_KEYMENU => 0,
        WM_KEYDOWN => {
            if app.settings_open {
                match w as u16 {
                    VK_ESCAPE => app.show_search(hwnd),
                    VK_RETURN | VK_SPACE => {
                        if let Some(index) = app.settings_cursor {
                            app.activate_row(hwnd, index);
                        }
                    }
                    VK_DOWN | VK_UP => app.step_cursor(hwnd, w as u16 == VK_DOWN),
                    _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
                }
                return 0;
            }
            match w as u16 {
                VK_ESCAPE => app.hide_panel(hwnd),
                VK_RETURN => app.open_selected(hwnd, unsafe { GetKeyState(VK_CONTROL as i32) } < 0),
                VK_DOWN | VK_UP => app.step_selection(w as u16 == VK_DOWN),
                _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
            }
            0
        }
        WM_CLOSE => {
            app.hide_panel(hwnd);
            0
        }
        WM_DESTROY => {
            unsafe {
                KillTimer(hwnd, RETRY_TIMER);
                KillTimer(hwnd, NOTICE_TIMER)
            };
            if app.hotkey_registered {
                unsafe { UnregisterHotKey(hwnd, HOTKEY_ID) };
            }
            unsafe { Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd)) };
            app.sender.take();
            app.results.take();
            // Dropping the stop end tells the workers the window is gone.
            app.sizes_results.take();
            app.sizes_stop.take();
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

fn run(start_hidden: bool) -> Result<(), String> {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES,
    };
    if unsafe { InitCommonControlsEx(&controls) } == 0 {
        return Err("cannot initialize common controls".into());
    }
    let instance = unsafe { GetModuleHandleW(null()) };
    let name = wide(CLASS);
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        lpszClassName: name.as_ptr(),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err("cannot register the window class".into());
    }
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            name.as_ptr(),
            wide("better_search").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            150,
            150,
            510,
            560,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    };
    if hwnd.is_null() {
        return Err(format!(
            "cannot create search window: {}",
            std::io::Error::last_os_error()
        ));
    }
    let app = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App };
    // The startup default: a square panel at the right edge, centred vertically.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
        let (x, y, width, height) = panel_rect(info.rcWork);
        unsafe { SetWindowPos(hwnd, null_mut(), x, y, width, height, SWP_NOZORDER) };
    }
    unsafe {
        if !start_hidden {
            ShowWindow(hwnd, SW_SHOW);
            SetFocus((*app).edit);
        }
    }
    // What this costs is asked for on every start, so Settings can show it even when the
    // offer was answered long ago. A postponed offer is asked again when it arrives.
    unsafe {
        (*app).start_sizes(hwnd);
        if !start_hidden {
            (*app).offer_windows_search(hwnd);
        }
    }
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg)
        };
    }
    unsafe { DestroyWindow(hwnd) };
    Ok(())
}

/// `--check-updates`: reports what the release host offers and exits, so the update
/// path can be checked from a script or a terminal without opening the panel.
/// Exit codes: 0 nothing newer, 2 something newer, 1 the check failed.
fn check_updates_here() -> i32 {
    match update::check() {
        Ok(Some(manifest)) => {
            println!(
                "better_search {} is available (installed {}): {}",
                manifest.version,
                update::current(),
                manifest.url
            );
            2
        }
        Ok(None) => {
            println!("better_search {} is the newest version.", update::current());
            0
        }
        Err(err) => {
            eprintln!("better_search: could not check for updates: {err}");
            1
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, state] = args.as_slice()
        && flag == winsearch::ARGUMENT
    {
        // The exit code is how the window that asked hears what happened.
        std::process::exit(winsearch::apply(state == "off") as i32);
    }
    if args.iter().any(|arg| arg == "--check-updates") {
        std::process::exit(check_updates_here());
    }
    let hidden = std::env::args_os()
        .skip(1)
        .any(|arg| arg == std::ffi::OsStr::new("--hidden"));
    // One panel per session: a second launch just brings the running one forward.
    let existing = unsafe { FindWindowW(wide(CLASS).as_ptr(), null()) };
    if !existing.is_null() {
        unsafe { PostMessageW(existing, WM_SHOW_PANEL, 0, 0) };
        return;
    }
    if let Err(err) = run(hidden) {
        let text = wide(&err);
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                null_mut(),
                text.as_ptr(),
                wide("better_search").as_ptr(),
                0,
            )
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_and_links_are_labelled_in_words() {
        let hit = |path: &str| Hit {
            path: path.into(),
            is_dir: false,
            score: 0,
        };
        let menu = hit(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Excel.lnk");
        assert_eq!(display_name(&menu), "Excel");
        assert_eq!(describe(&menu), "App");
        assert!(is_app(&menu));
        let desk = hit(r"C:\Users\bob\Desktop\Zoom.lnk");
        assert_eq!(describe(&desk), "App shortcut on the Desktop");
        let link = hit(r"C:\Users\bob\Downloads\Bank.url");
        assert_eq!(display_name(&link), "Bank");
        assert_eq!(describe(&link), r"Web link in C:\Users\bob\Downloads");
        assert!(!is_app(&link));
        assert_eq!(
            describe(&hit(r"C:\Windows\System32\cmd.exe")),
            "Windows tool"
        );
        assert_eq!(describe(&hit(r"D:\Tools\x.exe")), r"App in D:\Tools");
        let doc = hit(r"C:\Users\bob\Documents\a.pdf");
        assert_eq!(display_name(&doc), "a.pdf");
        assert_eq!(describe(&doc), r"C:\Users\bob\Documents");
        let folder = Hit {
            is_dir: true,
            ..hit(r"C:\Users\bob\Projects\")
        };
        assert_eq!(display_name(&folder), "Projects");
        assert_eq!(describe(&folder), r"C:\Users\bob");
        assert_eq!(kind_of(&folder), rows::Kind::Folders);
        assert_eq!(kind_of(&doc), rows::Kind::Documents);
        assert_eq!(kind_of(&menu), rows::Kind::Apps);
        assert_eq!(kind_of(&link), rows::Kind::Files);
        assert_eq!(status_text(1, 0), "1 result");
        assert_eq!(status_text(4, 2), "4 results · 2 more with Ctrl+H");
        let store = hit(r"shell:AppsFolder\Microsoft.Paint_8wekyb3d8bbwe!App");
        assert_eq!(describe(&store), "App");
        assert!(is_app(&store));
        assert_eq!(extension(&store), None);
    }

    #[test]
    fn default_panel_is_square_at_seventy_percent_height() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let (x, y, width, height) = panel_rect(work);
        assert_eq!(width, 728);
        assert_eq!(height, 728);
        assert_eq!(x, 1920 - width);
        assert_eq!(y, (1040 - height) / 2);
    }

    fn sizes(
        entries: Option<u64>,
        memory: Option<u64>,
        windows_memory: Option<u64>,
        windows_disk: Option<u64>,
    ) -> bs_pipe::StatsReply {
        bs_pipe::StatsReply {
            status: Status::Ok,
            service_private: memory,
            service_working_set: Some(15 * 1024 * 1024),
            index_heap: Some(18 * 1024 * 1024),
            snapshot_disk: Some(4_500_000),
            log_disk: Some(1_702),
            service_binary_disk: Some(2_000_000),
            entries,
            windows_search_memory: windows_memory,
            windows_search_disk: windows_disk,
        }
    }

    #[test]
    fn the_offer_is_short_and_plain() {
        let text = offer_text();
        // The question comes first, then what indexing is, in words a regular user
        // knows. No numbers: those live in Settings, where they can be checked.
        assert!(
            text.starts_with("Do you want to turn off the Windows indexing"),
            "{text}"
        );
        assert!(
            text.contains("better_search already keeps its own"),
            "{text}"
        );
        assert!(
            text.contains("you can turn indexing back on any time"),
            "{text}"
        );
        // The honest cost of saying yes: programs that search inside files.
        assert!(text.contains("search inside documents"), "{text}");
        assert!(!text.contains("MB"), "{text}");
    }

    #[test]
    fn the_settings_line_says_what_it_costs_and_where_the_index_is() {
        let ready = sizes_text(Some(&stats::Reading::Sizes(sizes(
            Some(574_455),
            Some(21 * 1024 * 1024),
            None,
            None,
        ))));
        assert!(
            ready.contains("Using 15 MB of memory and keeping a 4 MB index"),
            "{ready}"
        );
        assert!(ready.contains("for 574,455 files and folders"), "{ready}");
        assert!(ready.contains(r"C:\ProgramData\better_search"), "{ready}");
        // Before the first scan there is nothing to quote, and it says so.
        assert_eq!(sizes_text(None), "Measuring…");
        assert_eq!(
            sizes_text(Some(&stats::Reading::Scanning)),
            "Measuring once the first scan finishes…"
        );
    }
}
