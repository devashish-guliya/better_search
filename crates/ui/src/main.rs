#![windows_subsystem = "windows"]
//! Unelevated search UI. All index access goes through bs_pipe::Client in `search`.

mod apps;
mod commands;
mod draw;
mod frecency;
mod rows;
mod search;
mod settings;
mod thumbs;
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
    DEFAULT_GUI_FONT, DT_CENTER, DT_END_ELLIPSIS, DT_PATH_ELLIPSIS, DT_RIGHT, DeleteDC,
    DeleteObject, EndPaint, FillRect, GetDC, GetMonitorInfoW, GetStockObject, HBRUSH, HFONT,
    InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT,
    ReleaseDC, SRCCOPY, SelectObject, SetBkColor, SetTextColor,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETMARGINS, HKM_GETHOTKEY, HKM_SETHOTKEY, ICC_HOTKEY_CLASS,
    ICC_LISTVIEW_CLASSES, ILC_COLOR32, ILD_TRANSPARENT, INITCOMMONCONTROLSEX, ImageList_Add,
    ImageList_Create, ImageList_Destroy, ImageList_Draw, ImageList_Remove, ImageList_ReplaceIcon,
    InitCommonControlsEx, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVHITTESTINFO, LVIF_TEXT, LVIR_BOUNDS,
    LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_ENSUREVISIBLE, LVM_GETITEMRECT, LVM_GETNEXTITEM,
    LVM_HITTEST, LVM_INSERTCOLUMNW, LVM_REDRAWITEMS, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETIMAGELIST, LVM_SETITEMCOUNT, LVM_SETITEMSTATE, LVN_GETDISPINFOW, LVN_ITEMCHANGED,
    LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_NOCOLUMNHEADER, LVS_OWNERDATA,
    LVS_OWNERDRAWFIXED, LVS_REPORT, LVS_SHAREIMAGELISTS, LVS_SHOWSELALWAYS, LVS_SINGLESEL,
    LVSIL_SMALL, MEASUREITEMSTRUCT, NM_DBLCLK, NMHDR, NMLVDISPINFOW, ODS_SELECTED, SetWindowTheme,
    WC_LISTVIEWW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, RegisterHotKey, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    UnregisterHotKey, VK_BACK, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_TAB, VK_UP,
};

use draw::scale;
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    RemoveWindowSubclass, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
    SHGetFileInfoW, SetWindowSubclass, Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, EN_CHANGE,
    ES_AUTOHSCROLL, GWLP_USERDATA, GetClientRect, GetCursorPos, GetForegroundWindow, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW,
    IDI_APPLICATION, IsWindowVisible, KillTimer, LoadCursorW, LoadIconW, MF_STRING, MSG,
    MoveWindow, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOZORDER, SendMessageW, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WINDOWPOS, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND,
    WM_HOTKEY, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_MEASUREITEM,
    WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_NOTIFY, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SETFOCUS, WM_SETFONT, WM_SETTINGCHANGE, WM_SIZE, WM_THEMECHANGED, WM_TIMER,
    WM_WINDOWPOSCHANGED, WNDCLASSW, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP,
    WS_VISIBLE,
};
/// Sent when the user changes the Windows accent colour.
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const WM_MOUSELEAVE: u32 = 0x02a3;
const EM_GETSEL: u32 = 0x00b0;
const EM_SETSEL: u32 = 0x00b1;
/// Static control styles: vertically centred, single-line text cut with an ellipsis.
const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;

const CLASS: &str = "BetterSearchWindow";
const EDIT_ID: usize = 101;
const LIST_ID: usize = 102;
const STATUS_ID: usize = 103;
const TRAY_MESSAGE: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 2;
const HOTKEY_ID: i32 = 1;
const MENU_OPEN: usize = 201;
const MENU_SETTINGS: usize = 202;
const MENU_PAUSE: usize = 203;
const MENU_QUIT: usize = 204;
const MENU_RESULT_OPEN: usize = 210;
const MENU_RESULT_FOLDER: usize = 211;
const MENU_RESULT_COPY: usize = 212;
const RETRY_TIMER: usize = 3;
const NOTICE_TIMER: usize = 4;
const SETTINGS_HOTKEY: usize = 301;
const SETTINGS_STARTUP: usize = 304;
const SETTINGS_SAVE: usize = 305;
const SETTINGS_BACK: usize = 306;
const SETTINGS_HISTORY: usize = 307;
const SETTINGS_WIN_S: usize = 320;
const SETTINGS_WINDOWS_SEARCH: usize = 321;
const SETTINGS_CLEAR: usize = 308;
const CHECKED: isize = 1;
/// Result icons and previews, in pixels at 96 DPI.
const ICON_SIZE: i32 = 32;
/// Pictures kept before the image list starts over.
const MAX_IMAGES: usize = 600;
/// `EM_SETMARGINS` flags for the search box's inner text padding.
const EC_LEFTMARGIN: usize = 0x0001;
const EC_RIGHTMARGIN: usize = 0x0002;
const EM_SETCUEBANNER: u32 = 0x1501;
const KEY_HINTS: &str = "Ctrl+Enter  show in folder";

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
    settings: settings::Settings,
    history: frecency::History,
    paused: bool,
    /// Ctrl+H: also show matches inside system, app-data, and program folders.
    /// Per session, so the window always starts with the tidy view.
    include_system: bool,
    hotkey_registered: bool,
    settings_open: bool,
    controls: Vec<HWND>,
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
    /// Folder the search is limited to (Win+S over an Explorer window), shown as a
    /// chip in the field; Backspace at the start of the field removes it.
    scope: Option<String>,
    chip: RECT,
    /// When a letter typed in Start last arrived.
    start_typed_at: Option<std::time::Instant>,
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
            settings: settings::Settings::load(),
            history: frecency::History::load(),
            paused: false,
            include_system: false,
            hotkey_registered: false,
            settings_open: false,
            controls: Vec::new(),
            light: None,
            background: null_mut(),
            surface_brush: null_mut(),
            fonts: None,
            colors: draw::palette(true, settings::accent_color()),
            field: RECT::default(),
            footer: RECT::default(),
            show_hints: std::cell::Cell::new(true),
            taskbar_message: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
            scope: None,
            chip: RECT::default(),
            start_typed_at: None,
        }
    }

    /// What is sent to the service: the typed text, limited to the scope folder.
    fn request_text(&self) -> String {
        match &self.scope {
            Some(folder) => format!("in:\"{folder}\" {}", self.last_text),
            None => self.last_text.clone(),
        }
    }

    fn set_scope(&mut self, hwnd: HWND, scope: Option<String>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        let cue = wide(if self.scope.is_some() {
            "Search this folder"
        } else {
            "Search apps, files and folders"
        });
        unsafe { SendMessageW(self.edit, EM_SETCUEBANNER, 1, cue.as_ptr() as isize) };
        self.layout(hwnd);
        self.query(hwnd);
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
        } else if self.request_text().len() + 4 > bs_pipe::MAX_REQUEST {
            self.status = "The search is too long".into();
        } else if self.paused {
            self.status = "Search paused from the tray".into();
        } else {
            self.status = "Searching…".into();
            if let Some(sender) = &self.sender {
                let _ = sender.send(search::Request {
                    serial: self.serial,
                    text: self.request_text(),
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
                    let store: Vec<(Hit, String)> = if self.scope.is_some() {
                        Vec::new()
                    } else {
                        self.apps
                            .matches(&self.last_text)
                            .into_iter()
                            .filter(|(_, name)| !shortcut_names.contains(&name.to_lowercase()))
                            .chain(commands::matching(&self.last_text))
                            .collect()
                    };
                    let shown = reply.total_matches + store.len() as u32;
                    let hidden = reply.hidden_matches;
                    self.app_names.clear();
                    for (hit, name) in store {
                        self.app_names.insert(hit.path.clone(), name);
                        reply.hits.push(hit);
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
                    self.status = "The index is still loading".into();
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
            .cloned()
            .unwrap_or_else(|| display_name(hit))
    }

    fn update_title(&self, hwnd: HWND) {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                hwnd,
                wide("better_search").as_ptr(),
            )
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
        let key = if hit.is_dir {
            "<folder>".into()
        } else {
            extension.map_or_else(|| "<file>".into(), str::to_ascii_lowercase)
        };
        if let Some(&icon) = self.icon_cache.get(&key) {
            return icon;
        }
        let fake = if hit.is_dir {
            wide("folder")
        } else if let Some(extension) = extension {
            wide(&format!("file.{extension}"))
        } else {
            wide("file")
        };
        let mut info = SHFILEINFOW::default();
        let attrs = if hit.is_dir {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            FILE_ATTRIBUTE_NORMAL
        };
        // Use attributes, not actual disk I/O.
        let found = unsafe {
            SHGetFileInfoW(
                fake.as_ptr(),
                attrs,
                &mut info,
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
            )
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
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(self.edit, SW_HIDE);
            ShowWindow(self.list, SW_HIDE);
            ShowWindow(self.status_label, SW_HIDE);
        }
        for &control in &self.controls {
            unsafe { ShowWindow(control, SW_SHOW) };
        }
        let hotkey = encode_hotkey(self.settings.modifiers, self.settings.key);
        unsafe {
            SendMessageW(self.controls[1], HKM_SETHOTKEY, hotkey, 0);
            SendMessageW(
                self.controls[2],
                BM_SETCHECK,
                self.settings.start_with_windows as usize,
                0,
            );
            SendMessageW(
                self.controls[3],
                BM_SETCHECK,
                self.settings.history as usize,
                0,
            );
            SendMessageW(
                self.controls[4],
                BM_SETCHECK,
                self.settings.win_s as usize,
                0,
            );
            SendMessageW(
                self.controls[5],
                BM_SETCHECK,
                winsearch::is_off() as usize,
                0,
            );
            SetForegroundWindow(hwnd);
            SetFocus(self.controls[1]);
        }
        self.layout(hwnd);
        let title = wide("better_search  ·  Settings");
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(hwnd, title.as_ptr())
        };
    }

    fn show_search(&mut self, hwnd: HWND) {
        self.settings_open = false;
        for &control in &self.controls {
            unsafe { ShowWindow(control, SW_HIDE) };
        }
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
    }

    fn hide_panel(&mut self, hwnd: HWND) {
        unsafe {
            KillTimer(hwnd, RETRY_TIMER);
            ShowWindow(hwnd, SW_HIDE);
        }
    }

    fn save_settings(&mut self, hwnd: HWND) {
        let hotkey = unsafe { SendMessageW(self.controls[1], HKM_GETHOTKEY, 0, 0) } as u32;
        let Some((modifiers, key)) = decode_hotkey(hotkey) else {
            message(
                hwnd,
                "Choose a key with Alt, Ctrl, Shift, or a combination.",
            );
            return;
        };
        let next = settings::Settings {
            key,
            modifiers,
            start_with_windows: unsafe { SendMessageW(self.controls[2], BM_GETCHECK, 0, 0) }
                == CHECKED,
            history: unsafe { SendMessageW(self.controls[3], BM_GETCHECK, 0, 0) } == CHECKED,
            win_s: unsafe { SendMessageW(self.controls[4], BM_GETCHECK, 0, 0) } == CHECKED,
        };
        if next.key != self.settings.key || next.modifiers != self.settings.modifiers {
            if self.hotkey_registered {
                unsafe { UnregisterHotKey(hwnd, HOTKEY_ID) };
            }
            if unsafe { RegisterHotKey(hwnd, HOTKEY_ID, next.modifiers, next.key) } == 0 {
                self.hotkey_registered = unsafe {
                    RegisterHotKey(hwnd, HOTKEY_ID, self.settings.modifiers, self.settings.key)
                } != 0;
                message(hwnd, "That hotkey is already in use. Choose another.");
                return;
            }
            self.hotkey_registered = true;
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
        let search_off = unsafe { SendMessageW(self.controls[5], BM_GETCHECK, 0, 0) } == CHECKED;
        if search_off != winsearch::is_off() && !winsearch::request(hwnd, search_off) {
            message(
                hwnd,
                "Windows search was not changed. Approve the administrator prompt to change it.",
            );
        }
        winkey::set_search_off(winsearch::is_off());
        self.settings = next;
        winkey::set_enabled(self.settings.win_s);
        self.show_search(hwnd);
    }

    fn layout(&mut self, hwnd: HWND) {
        let mut rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rect) };
        let pad = scale(hwnd, draw::SPACE_XL);
        let width = (rect.right - 2 * pad).max(0);
        if self.settings_open {
            self.field = RECT::default();
            self.footer = RECT::default();
            let line = scale(hwnd, 35);
            let top = scale(hwnd, 20);
            let mut y = top;
            for (i, &control) in self.controls.iter().enumerate() {
                let height = if i == 8 {
                    line * 2
                } else {
                    line - scale(hwnd, 3)
                };
                unsafe { MoveWindow(control, pad, y, width, height, 1) };
                y += height + scale(hwnd, 3);
            }
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
        let mut edit_left = self.field.left + s(40);
        self.chip = RECT::default();
        if let (Some(scope), Some(fonts)) = (&self.scope, &self.fonts) {
            let label: Vec<u16> = scope_label(scope).encode_utf16().collect();
            let dc = unsafe { GetDC(hwnd) };
            let text_width = draw::text_width(dc, fonts.detail, &label);
            unsafe { ReleaseDC(hwnd, dc) };
            let width = (text_width + 2 * s(draw::SPACE_M)).min(width / 2);
            let height = s(26);
            let top = self.field.top + (s(40) - height) / 2;
            self.chip = RECT {
                left: edit_left,
                top,
                right: edit_left + width,
                bottom: top + height,
            };
            edit_left = self.chip.right + s(draw::SPACE_S);
        }
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
                (self.field.right - s(draw::SPACE_L) - edit_left).max(0),
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
        for &control in &self.controls {
            assign(control, fonts.ui);
        }
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
            VK_BACK if id == EDIT_ID && app.scope.is_some() => {
                let (mut start, mut end) = (0u32, 0u32);
                unsafe {
                    SendMessageW(
                        hwnd,
                        EM_GETSEL,
                        (&raw mut start) as usize,
                        (&raw mut end) as isize,
                    )
                };
                if start == 0 && end == 0 {
                    app.set_scope(main, None);
                    return 0;
                }
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

/// Shows the window for a key the hook caught. Unlike a registered hotkey, a hook
/// does not give the right to take the foreground, so it is borrowed from `front`.
fn open_from_hook(app: &mut App, hwnd: HWND, front: HWND, over_start: bool) {
    let mut front = front;
    // Start keeps the foreground against every other program, so it is closed first.
    if over_start && winkey::is_start(unsafe { GetForegroundWindow() }) {
        winkey::close_start();
        front = unsafe { GetForegroundWindow() };
    }
    winkey::tap_unassigned_key();
    let front_thread = unsafe { GetWindowThreadProcessId(front, null_mut()) };
    let own_thread = unsafe { GetCurrentThreadId() };
    let attached = front_thread != own_thread
        && unsafe { AttachThreadInput(own_thread, front_thread, 1) } != 0;
    app.open_panel(hwnd);
    if attached {
        unsafe { AttachThreadInput(own_thread, front_thread, 0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn encode_hotkey(modifiers: u32, key: u32) -> usize {
    (key as usize)
        | (usize::from(modifiers & 4 != 0) << 8)
        | (usize::from(modifiers & 2 != 0) << 9)
        | (usize::from(modifiers & 1 != 0) << 10)
}

fn decode_hotkey(value: u32) -> Option<(u32, u32)> {
    let key = value & 0xff;
    let flags = (value >> 8) & 0xff;
    let modifiers = ((flags & 1) << 2) | (flags & 2) | ((flags & 4) >> 2);
    (key != 0 && modifiers != 0).then_some((modifiers, key))
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

/// The scope chip's text: the folder's own name, or the drive for a root.
fn scope_label(folder: &str) -> String {
    let folder = folder.trim_end_matches('\\');
    format!("In {}", folder.rsplit('\\').next().unwrap_or(folder))
}

/// The folder holding `path`, or the path itself for a drive root.
fn parent_folder(path: &str) -> &str {
    let path = path.trim_end_matches('\\');
    path.rsplit_once('\\').map_or(path, |(parent, _)| parent)
}

/// The second line: what an app or link is, in words, or the folder a file is in.
fn describe(hit: &Hit) -> String {
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
            let static_class = wide("STATIC");
            let button_class = wide("BUTTON");
            app.controls = vec![
                control(hwnd, &static_class, "Hotkey (Alt+Space by default):", 0, 0),
                control(
                    hwnd,
                    &wide("msctls_hotkey32"),
                    "",
                    SETTINGS_HOTKEY,
                    WS_BORDER,
                ),
                control(
                    hwnd,
                    &button_class,
                    "Start with Windows (current user only)",
                    SETTINGS_STARTUP,
                    BS_AUTOCHECKBOX as u32,
                ),
                control(
                    hwnd,
                    &button_class,
                    "Rank what I open higher (kept on this PC)",
                    SETTINGS_HISTORY,
                    BS_AUTOCHECKBOX as u32,
                ),
                control(
                    hwnd,
                    &button_class,
                    "Win+S and typing in Start open better_search",
                    SETTINGS_WIN_S,
                    BS_AUTOCHECKBOX as u32,
                ),
                control(
                    hwnd,
                    &button_class,
                    "Turn off Windows search and its indexer (asks for admin)",
                    SETTINGS_WINDOWS_SEARCH,
                    BS_AUTOCHECKBOX as u32,
                ),
                control(hwnd, &button_class, "Clear open history", SETTINGS_CLEAR, 0),
                control(
                    hwnd,
                    &static_class,
                    &format!(
                        "Service drives: {} (all fixed NTFS; selection deferred)",
                        settings::indexed_drives()
                    ),
                    0,
                    0,
                ),
                control(
                    hwnd,
                    &static_class,
                    "Skipped folder counts and per-folder overrides are deferred; the service does not index their contents.",
                    0,
                    0,
                ),
                control(hwnd, &button_class, "Save settings", SETTINGS_SAVE, 0),
                control(hwnd, &button_class, "Back to search", SETTINGS_BACK, 0),
            ];
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
            winkey::install(hwnd, app.settings.win_s, winsearch::is_off());
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
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let device = unsafe { BeginPaint(hwnd, &mut paint) };
            let mut rect = RECT::default();
            unsafe { GetClientRect(hwnd, &mut rect) };
            unsafe { FillRect(device, &rect, app.background) };
            if let Some(fonts) = &app.fonts
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
                if let Some(scope) = &app.scope
                    && app.chip.right > app.chip.left
                {
                    let chip = app.chip;
                    draw::fill_round(device, &chip, chip.bottom - chip.top, colors.selected);
                    let text = RECT {
                        left: chip.left + s(draw::SPACE_M),
                        right: chip.right - s(draw::SPACE_M),
                        ..chip
                    };
                    draw::text(
                        device,
                        &scope_label(scope),
                        &text,
                        fonts.detail,
                        colors.text,
                        DT_CENTER | DT_END_ELLIPSIS,
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
                SETTINGS_SAVE => app.save_settings(hwnd),
                SETTINGS_BACK => app.show_search(hwnd),
                SETTINGS_CLEAR => match app.history.clear() {
                    Ok(()) => message(hwnd, "Open history cleared."),
                    Err(err) => message(hwnd, &format!("Could not clear the history: {err}")),
                },
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
                        (MENU_QUIT, "Quit"),
                    ],
                ),
                _ => {}
            }
            0
        }
        WM_HOTKEY if w as i32 == HOTKEY_ID => {
            app.set_scope(hwnd, None);
            app.open_panel(hwnd);
            0
        }
        winkey::WM_WIN_S => {
            // Still the window the user was in: this one has not been activated yet.
            let front = unsafe { GetForegroundWindow() };
            let scope = if front == hwnd {
                app.scope.clone()
            } else {
                winkey::explorer_folder(front)
            };
            if scope.is_some() {
                unsafe { SetWindowTextW(app.edit, wide("").as_ptr()) };
            }
            app.set_scope(hwnd, scope);
            open_from_hook(app, hwnd, front, false);
            0
        }
        winkey::WM_START_TYPED => {
            let front = unsafe { GetForegroundWindow() };
            let typed = char::from_u32(w as u32).unwrap_or(' ');
            // Letters typed before this window took over from Start follow the first.
            let continuing = front == hwnd
                || app
                    .start_typed_at
                    .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(1500));
            app.start_typed_at = Some(std::time::Instant::now());
            let text = if continuing {
                format!("{}{typed}", app.edit_text())
            } else {
                app.set_scope(hwnd, None);
                typed.to_string()
            };
            unsafe {
                SetWindowTextW(app.edit, wide(&text).as_ptr());
                let end = text.encode_utf16().count();
                SendMessageW(app.edit, EM_SETSEL, end, end as isize);
            }
            if front != hwnd {
                open_from_hook(app, hwnd, front, true);
            }
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
                        text: app.request_text(),
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
        WM_KEYDOWN => {
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
        dwICC: ICC_LISTVIEW_CLASSES | ICC_HOTKEY_CLASS,
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, state] = args.as_slice()
        && flag == winsearch::ARGUMENT
    {
        winsearch::apply(state == "off");
        return;
    }
    let hidden = std::env::args_os()
        .skip(1)
        .any(|arg| arg == std::ffi::OsStr::new("--hidden"));
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

    #[test]
    fn hotkey_control_round_trips_alt_space_and_combinations() {
        for (modifiers, key) in [(1, 32), (3, 75), (7, 90)] {
            assert_eq!(
                decode_hotkey(encode_hotkey(modifiers, key) as u32),
                Some((modifiers, key))
            );
        }
        assert_eq!(decode_hotkey(32), None);
        assert_eq!(decode_hotkey(4 << 8), None);
    }
}
