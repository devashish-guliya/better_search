#![windows_subsystem = "windows"]
//! Unelevated search UI. All index access goes through bs_pipe::Client in `search`.

mod hover;
mod search;
mod settings;

use std::collections::HashMap;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{Receiver, Sender};

use bs_pipe::{Hit, Status};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreatePen, CreateSolidBrush,
    DEFAULT_CHARSET, DEFAULT_GUI_FONT, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FW_NORMAL, FillRect, GetDC, GetMonitorInfoW, GetStockObject,
    GetTextFaceW, HBRUSH, HFONT, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, NULL_BRUSH, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, ReleaseDC, RoundRect,
    SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    EM_SETMARGINS, HKM_GETHOTKEY, HKM_SETHOTKEY, ICC_HOTKEY_CLASS, ICC_LISTVIEW_CLASSES,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_IMAGE,
    LVIF_TEXT, LVIS_SELECTED, LVITEMW, LVM_ENSUREVISIBLE, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETIMAGELIST, LVM_SETITEMCOUNT, LVM_SETITEMSTATE,
    LVN_GETDISPINFOW, LVN_ITEMCHANGED, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
    LVS_EX_LABELTIP, LVS_OWNERDATA, LVS_REPORT, LVS_SHAREIMAGELISTS, LVS_SHOWSELALWAYS,
    LVS_SINGLESEL, LVSIL_SMALL, NM_DBLCLK, NMHDR, NMLVDISPINFOW, SetWindowTheme, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, RegisterHotKey, SetFocus, UnregisterHotKey, VK_CONTROL, VK_DOWN,
    VK_ESCAPE, VK_RETURN, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    RemoveWindowSubclass, SHFILEINFOW, SHGFI_SMALLICON, SHGFI_SYSICONINDEX,
    SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SetWindowSubclass, Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, EN_CHANGE, ES_AUTOHSCROLL,
    GWLP_USERDATA, GetClientRect, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, IDC_ARROW, IDI_APPLICATION, IsWindowVisible, KillTimer,
    LoadCursorW, LoadIconW, MF_STRING, MSG, MoveWindow, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SW_HIDE, SW_SHOW, SWP_NOZORDER, SendMessageW, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_HOTKEY,
    WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_NCCREATE, WM_NCDESTROY, WM_NOTIFY, WM_PAINT,
    WM_RBUTTONUP, WM_SETFOCUS, WM_SETFONT, WM_SETTINGCHANGE, WM_SIZE, WM_THEMECHANGED, WM_TIMER,
    WNDCLASSW, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};

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
const ANIMATION_TIMER: usize = 2;
const RETRY_TIMER: usize = 3;
const SETTINGS_HOTKEY: usize = 301;
const SETTINGS_HOVER: usize = 302;
const SETTINGS_SIDE: usize = 303;
const SETTINGS_STARTUP: usize = 304;
const SETTINGS_SAVE: usize = 305;
const SETTINGS_BACK: usize = 306;
const CHECKED: isize = 1;
/// `EM_SETMARGINS` flags for the search box's inner text padding.
const EC_LEFTMARGIN: usize = 0x0001;
const EC_RIGHTMARGIN: usize = 0x0002;

/// Colors are Win32 `COLORREF`s: 0x00BBGGRR.
fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

/// A small, flat palette so the window looks the same on light and dark Windows.
/// The list shares `panel` so the result card's rounded outline has no square corners.
struct Palette {
    panel: u32,
    text: u32,
    edge: u32,
}

fn palette(light: bool) -> Palette {
    if light {
        Palette {
            panel: rgb(250, 250, 250),
            text: rgb(24, 24, 24),
            edge: rgb(206, 206, 206),
        }
    } else {
        Palette {
            panel: rgb(26, 26, 26),
            text: rgb(240, 240, 240),
            edge: rgb(72, 72, 72),
        }
    }
}

struct Slide {
    from: i32,
    to: i32,
    top: i32,
    width: i32,
    height: i32,
    frame: i32,
}

struct App {
    edit: HWND,
    list: HWND,
    status_label: HWND,
    sender: Option<Sender<search::Request>>,
    results: Option<Receiver<search::ResultMessage>>,
    serial: u64,
    hits: Vec<Hit>,
    names: Vec<Vec<u16>>,
    paths: Vec<Vec<u16>>,
    status: String,
    last_text: String,
    icon_cache: HashMap<String, i32>,
    image_list: isize,
    settings: settings::Settings,
    paused: bool,
    hotkey_registered: bool,
    hover: hover::Hover,
    slide: Option<Slide>,
    settings_open: bool,
    controls: Vec<HWND>,
    light: Option<bool>,
    background: HBRUSH,
    font: HFONT,
    panel: u32,
    text: u32,
    edge: u32,
    outlines: Vec<RECT>,
    taskbar_message: u32,
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
            names: Vec::new(),
            paths: Vec::new(),
            status: "Type to search".into(),
            last_text: String::new(),
            icon_cache: HashMap::new(),
            image_list: 0,
            settings: settings::Settings::load(),
            paused: false,
            hotkey_registered: false,
            hover: hover::Hover::new(),
            slide: None,
            settings_open: false,
            controls: Vec::new(),
            light: None,
            background: null_mut(),
            font: null_mut(),
            panel: rgb(250, 250, 250),
            text: rgb(24, 24, 24),
            edge: rgb(206, 206, 206),
            outlines: Vec::new(),
            taskbar_message: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
        }
    }

    fn query(&mut self, hwnd: HWND) {
        let len = unsafe { GetWindowTextLengthW(self.edit) };
        let mut text = vec![0u16; len.max(0) as usize + 1];
        unsafe { GetWindowTextW(self.edit, text.as_mut_ptr(), text.len() as i32) };
        let query = String::from_utf16_lossy(&text[..len.max(0) as usize]);
        self.last_text = query.clone();
        self.serial = self.serial.wrapping_add(1);
        unsafe { KillTimer(hwnd, RETRY_TIMER) };
        self.clear_results();
        if query.trim().is_empty() {
            self.status = "Type a file or folder name".into();
        } else if query.len() + 4 > bs_pipe::MAX_REQUEST {
            self.status = "The search is too long".into();
        } else if self.paused {
            self.status = "Search paused from the tray".into();
        } else {
            self.status = "Searching…".into();
            if let Some(sender) = &self.sender {
                let _ = sender.send(search::Request {
                    serial: self.serial,
                    text: query,
                });
            }
        }
        self.update_title(hwnd);
    }

    fn clear_results(&mut self) {
        self.hits.clear();
        self.names.clear();
        self.paths.clear();
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
                    self.status = if reply.total_matches == 0 {
                        "No matches".into()
                    } else {
                        format!("{} matches", reply.total_matches)
                    };
                    self.names = reply
                        .hits
                        .iter()
                        .map(|hit| wide(hit.path.rsplit('\\').next().unwrap_or(&hit.path)))
                        .collect();
                    self.paths = reply.hits.iter().map(|hit| wide(&hit.path)).collect();
                    self.hits = reply.hits;
                    unsafe {
                        SendMessageW(self.list, LVM_SETITEMCOUNT, self.hits.len(), 0);
                    }
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

    fn update_title(&self, hwnd: HWND) {
        let title = wide(&format!("better_search  ·  {}", self.status));
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(hwnd, title.as_ptr())
        };
        if !self.status_label.is_null() {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                    self.status_label,
                    wide(&self.status).as_ptr(),
                )
            };
        }
    }

    fn selected(&self) -> Option<usize> {
        let index =
            unsafe { SendMessageW(self.list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as _) };
        (index >= 0 && (index as usize) < self.hits.len()).then_some(index as usize)
    }

    fn select(&self, index: usize) {
        if index >= self.hits.len() {
            return;
        }
        let clear = LVITEMW {
            state: 0,
            stateMask: LVIS_SELECTED,
            ..Default::default()
        };
        let select = LVITEMW {
            state: LVIS_SELECTED,
            stateMask: LVIS_SELECTED,
            ..Default::default()
        };
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
                index,
                (&select as *const LVITEMW) as _,
            );
            SendMessageW(self.list, LVM_ENSUREVISIBLE, index, 0);
        }
    }

    fn icon(&mut self, index: usize) -> i32 {
        let hit = &self.hits[index];
        let name = hit.path.rsplit('\\').next().unwrap_or(&hit.path);
        let extension = (!hit.is_dir)
            .then(|| name.rsplit_once('.').map(|(_, ext)| ext))
            .flatten();
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
        // Use attributes, not actual disk I/O. The system image list owns the icons.
        let images = unsafe {
            SHGetFileInfoW(
                fake.as_ptr(),
                attrs,
                &mut info,
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_SYSICONINDEX | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
            )
        };
        if images != 0 && self.image_list == 0 {
            self.image_list = images as isize;
            unsafe {
                SendMessageW(
                    self.list,
                    LVM_SETIMAGELIST,
                    LVSIL_SMALL as _,
                    self.image_list,
                )
            };
        }
        if self.icon_cache.len() >= 512 {
            self.icon_cache.clear();
        }
        self.icon_cache.insert(key, info.iIcon);
        info.iIcon
    }

    fn open_selected(&self, hwnd: HWND, folder: bool) {
        let Some(index) = self.selected() else { return };
        let hit = &self.hits[index];
        let (file, args) = if folder {
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

    fn slide_in(&mut self, hwnd: HWND) {
        if unsafe { IsWindowVisible(hwnd) } != 0 {
            unsafe { SetForegroundWindow(hwnd) };
            return;
        }
        if self.settings_open {
            self.show_search(hwnd);
        }
        let mut cursor = POINT::default();
        unsafe { GetCursorPos(&mut cursor) };
        let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return;
        }
        let work = info.rcWork;
        let width = scale(hwnd, 490).min(work.right - work.left);
        let height = scale(hwnd, 550).min(work.bottom - work.top);
        let left = self.settings.left;
        let from = if left { work.left - width } else { work.right };
        let to = if left { work.left } else { work.right - width };
        let top = work.top + scale(hwnd, 24);
        self.slide = Some(Slide {
            from,
            to,
            top,
            width,
            height,
            frame: 0,
        });
        unsafe {
            SetWindowPos(hwnd, null_mut(), from, top, width, height, SWP_NOZORDER);
            ShowWindow(hwnd, SW_SHOW);
            SetTimer(hwnd, ANIMATION_TIMER, 15, None);
        }
        if !self.last_text.trim().is_empty() {
            self.query(hwnd);
        }
    }

    fn advance_slide(&mut self, hwnd: HWND) {
        let Some(slide) = &mut self.slide else { return };
        slide.frame += 1;
        let fraction = slide.frame.min(12);
        let x = slide.from + (slide.to - slide.from) * fraction * (24 - fraction) / 144;
        unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                x,
                slide.top,
                slide.width,
                slide.height,
                SWP_NOZORDER,
            )
        };
        if fraction == 12 {
            unsafe {
                KillTimer(hwnd, ANIMATION_TIMER);
                SetForegroundWindow(hwnd);
                SetFocus(self.edit)
            };
            self.slide = None;
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
                self.settings.hover as usize,
                0,
            );
            SendMessageW(
                self.controls[3],
                BM_SETCHECK,
                self.settings.left as usize,
                0,
            );
            SendMessageW(
                self.controls[4],
                BM_SETCHECK,
                self.settings.start_with_windows as usize,
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
            KillTimer(hwnd, ANIMATION_TIMER);
            KillTimer(hwnd, RETRY_TIMER);
            ShowWindow(hwnd, SW_HIDE);
        }
        self.slide = None;
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
            hover: unsafe { SendMessageW(self.controls[2], BM_GETCHECK, 0, 0) } == CHECKED,
            left: unsafe { SendMessageW(self.controls[3], BM_GETCHECK, 0, 0) } == CHECKED,
            start_with_windows: unsafe { SendMessageW(self.controls[4], BM_GETCHECK, 0, 0) }
                == CHECKED,
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
        self.settings = next;
        self.hover
            .rebuild(hwnd, self.settings.hover, self.settings.left);
        self.show_search(hwnd);
    }

    fn layout(&mut self, hwnd: HWND) {
        let mut rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rect) };
        let pad = scale(hwnd, 14);
        let width = (rect.right - 2 * pad).max(0);
        self.outlines.clear();
        if self.settings_open {
            let line = scale(hwnd, 35);
            let top = scale(hwnd, 20);
            let mut y = top;
            for (i, &control) in self.controls.iter().enumerate() {
                let height = if i == 6 {
                    line * 2
                } else {
                    line - scale(hwnd, 3)
                };
                unsafe { MoveWindow(control, pad, y, width, height, 1) };
                y += height + scale(hwnd, 3);
            }
            return;
        }
        let edit_height = scale(hwnd, 38);
        let gap = scale(hwnd, 12);
        let status_height = scale(hwnd, 20);
        let list_top = pad + edit_height + gap;
        let list_height = (rect.bottom - pad - status_height - scale(hwnd, 8) - list_top).max(0);
        let inset = scale(hwnd, 10) as isize;
        unsafe {
            MoveWindow(self.edit, pad, pad, width, edit_height, 1);
            SendMessageW(
                self.edit,
                EM_SETMARGINS,
                EC_LEFTMARGIN | EC_RIGHTMARGIN,
                (inset | (inset << 16)) as _,
            );
            MoveWindow(self.list, pad, list_top, width, list_height, 1);
            MoveWindow(
                self.status_label,
                pad,
                rect.bottom - pad - status_height,
                width,
                status_height,
                1,
            );
            let name_width = (width / 3).max(scale(hwnd, 120));
            SendMessageW(
                self.list,
                windows_sys::Win32::UI::Controls::LVM_SETCOLUMNWIDTH,
                0,
                name_width as _,
            );
            SendMessageW(
                self.list,
                windows_sys::Win32::UI::Controls::LVM_SETCOLUMNWIDTH,
                1,
                (width - name_width - scale(hwnd, 24)).max(0) as _,
            );
        }
        // Rounded outlines the window's WM_PAINT draws around the field and the list.
        let radius = scale(hwnd, 5);
        self.outlines.push(RECT {
            left: pad - radius,
            top: pad - radius,
            right: pad + width + radius,
            bottom: pad + edit_height + radius,
        });
        self.outlines.push(RECT {
            left: pad - 1,
            top: list_top - 1,
            right: pad + width + 1,
            bottom: list_top + list_height + 1,
        });
    }

    /// Rebuilds the interface font for the current DPI and hands it to every child.
    fn apply_font(&mut self, hwnd: HWND) {
        if !self.font.is_null() {
            unsafe { DeleteObject(self.font) };
        }
        self.font = create_ui_font(hwnd);
        let font = self.font;
        for control in [self.edit, self.list, self.status_label]
            .into_iter()
            .chain(self.controls.iter().copied())
        {
            if !control.is_null() {
                unsafe { SendMessageW(control, WM_SETFONT, font as usize, 1) };
            }
        }
    }

    /// The bottom line shows the highlighted result's full path, or the search state.
    fn refresh_status(&self) {
        if self.status_label.is_null() {
            return;
        }
        let text = match self.selected().and_then(|index| self.hits.get(index)) {
            Some(hit) => hit.path.clone(),
            None => self.status.clone(),
        };
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                self.status_label,
                wide(&text).as_ptr(),
            )
        };
    }

    fn theme(&mut self, hwnd: HWND) {
        let light = settings::light_theme();
        if self.light == Some(light) {
            return;
        }
        self.light = Some(light);
        let colors = palette(light);
        self.panel = colors.panel;
        self.text = colors.text;
        self.edge = colors.edge;
        if !self.background.is_null() {
            unsafe { DeleteObject(self.background) };
        }
        self.background = unsafe { CreateSolidBrush(colors.panel) };
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
    if (msg == WM_SETFOCUS || msg == WM_KILLFOCUS) && id == EDIT_ID {
        // Repaint so the placeholder appears when the box loses focus and clears on focus.
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }
    if msg == WM_PAINT && id == EDIT_ID {
        // Let the edit paint itself, then add a placeholder when it is empty and idle.
        let result = unsafe { DefSubclassProc(hwnd, msg, w, l) };
        let idle = unsafe { GetFocus() } != hwnd && unsafe { GetWindowTextLengthW(hwnd) } == 0;
        if idle {
            let mut paint = PAINTSTRUCT::default();
            let device = unsafe { BeginPaint(hwnd, &mut paint) };
            let mut rect = RECT::default();
            unsafe { GetClientRect(hwnd, &mut rect) };
            rect.left += scale(hwnd, 12);
            let placeholder = wide("Search files and folders");
            unsafe {
                SetBkMode(device, TRANSPARENT as i32);
                SetTextColor(device, 0x00808080);
                DrawTextW(
                    device,
                    placeholder.as_ptr(),
                    -1,
                    &mut rect,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
                EndPaint(hwnd, &paint);
            }
        }
        return result;
    }
    if msg == WM_KEYDOWN {
        let main = parent as HWND;
        let ptr = unsafe { GetWindowLongPtrW(main, GWLP_USERDATA) as *mut App };
        if !ptr.is_null() {
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
                VK_DOWN | VK_UP if id == EDIT_ID => {
                    let current = app.selected().unwrap_or(0);
                    let next = if w as u16 == VK_DOWN {
                        (current + 1).min(app.hits.len().saturating_sub(1))
                    } else {
                        current.saturating_sub(1)
                    };
                    app.select(next);
                    return 0;
                }
                _ => {}
            }
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, w, l) }
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

fn scale(hwnd: HWND, value: i32) -> i32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    value * dpi as i32 / 96
}

/// Creates the interface font. Windows 11's Segoe UI Variable looks best; older
/// systems fall back to Segoe UI, which every supported Windows has.
fn create_ui_font(hwnd: HWND) -> HFONT {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
    // 10 pt, negative height for character height (not cell height).
    let height = -(10 * dpi / 72);
    let font = make_font(height, "Segoe UI Variable Text");
    if font_face_matches(hwnd, font, "Segoe UI Variable") {
        font
    } else {
        unsafe { DeleteObject(font) };
        make_font(height, "Segoe UI")
    }
}

fn make_font(height: i32, face: &str) -> HFONT {
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            0,
            wide(face).as_ptr(),
        )
    }
}

fn font_face_matches(hwnd: HWND, font: HFONT, needle: &str) -> bool {
    let device = unsafe { GetDC(hwnd) };
    if device.is_null() {
        return false;
    }
    let previous = unsafe { SelectObject(device, font) };
    let mut buffer = [0u16; 64];
    let length = unsafe { GetTextFaceW(device, buffer.len() as i32, buffer.as_mut_ptr()) };
    unsafe {
        SelectObject(device, previous);
        ReleaseDC(hwnd, device);
    }
    let end = (length.max(1) as usize - 1).min(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).contains(needle)
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
            if !app.background.is_null() {
                DeleteObject(app.background);
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
            app.status_label = control(hwnd, &wide("STATIC"), "", STATUS_ID, WS_VISIBLE);
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
                    "Open on screen-edge hover",
                    SETTINGS_HOVER,
                    BS_AUTOCHECKBOX as u32,
                ),
                control(
                    hwnd,
                    &button_class,
                    "Use left edge (unchecked: right)",
                    SETTINGS_SIDE,
                    BS_AUTOCHECKBOX as u32,
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
                    (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as _,
                );
            }
            let label = wide("Name");
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: scale(hwnd, 160),
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
            let path_label = wide("Path");
            let path_col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: scale(hwnd, 280),
                pszText: path_label.as_ptr() as _,
                ..Default::default()
            };
            unsafe {
                SendMessageW(
                    app.list,
                    LVM_INSERTCOLUMNW,
                    1,
                    (&path_col as *const LVCOLUMNW) as _,
                )
            };
            let (sender, results) = search::start(hwnd);
            app.sender = Some(sender);
            app.results = Some(results);
            app.theme(hwnd);
            app.register_hotkey(hwnd);
            unsafe { Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd)) };
            app.update_title(hwnd);
            0
        }
        WM_SIZE => {
            app.layout(hwnd);
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            app.theme(hwnd);
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let device = unsafe { BeginPaint(hwnd, &mut paint) };
            let mut rect = RECT::default();
            unsafe { GetClientRect(hwnd, &mut rect) };
            unsafe { FillRect(device, &rect, app.background) };
            if !app.outlines.is_empty() {
                let diameter = scale(hwnd, 14);
                let pen = unsafe { CreatePen(PS_SOLID, 1, app.edge) };
                unsafe {
                    let previous_pen = SelectObject(device, pen);
                    let previous_brush = SelectObject(device, GetStockObject(NULL_BRUSH));
                    for outline in &app.outlines {
                        RoundRect(
                            device,
                            outline.left,
                            outline.top,
                            outline.right,
                            outline.bottom,
                            diameter,
                            diameter,
                        );
                    }
                    SelectObject(device, previous_brush);
                    SelectObject(device, previous_pen);
                    DeleteObject(pen);
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
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            unsafe {
                SetBkColor(w as _, app.panel);
                SetTextColor(w as _, app.text);
            }
            app.background as _
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
                MENU_RESULT_COPY => app.copy_selected(hwnd),
                SETTINGS_SAVE => app.save_settings(hwnd),
                SETTINGS_BACK => app.show_search(hwnd),
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
            app.open_panel(hwnd);
            0
        }
        hover::WM_HOVER_TRIGGER => {
            app.slide_in(hwnd);
            0
        }
        WM_TIMER if w == ANIMATION_TIMER => {
            app.advance_slide(hwnd);
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
                    });
                }
            }
            0
        }
        WM_DISPLAYCHANGE => {
            app.hover
                .rebuild(hwnd, app.settings.hover, app.settings.left);
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
                    let disp = unsafe { &mut *(l as *mut NMLVDISPINFOW) };
                    let index = disp.item.iItem as usize;
                    if index < app.hits.len() {
                        if disp.item.mask & LVIF_TEXT != 0
                            && !disp.item.pszText.is_null()
                            && disp.item.cchTextMax > 0
                        {
                            let text = if disp.item.iSubItem == 0 {
                                &app.names[index]
                            } else {
                                &app.paths[index]
                            };
                            let count = text.len().min(disp.item.cchTextMax as usize);
                            unsafe {
                                std::ptr::copy_nonoverlapping(
                                    text.as_ptr(),
                                    disp.item.pszText,
                                    count,
                                );
                                *disp.item.pszText.add(count - 1) = 0;
                            }
                        }
                        if disp.item.mask & LVIF_IMAGE != 0 && disp.item.iSubItem == 0 {
                            disp.item.iImage = app.icon(index);
                        }
                    }
                    return 0;
                }
                if hdr.code == LVN_ITEMCHANGED {
                    app.refresh_status();
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
                                (MENU_RESULT_FOLDER, "Open folder"),
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
                VK_DOWN | VK_UP => {
                    let current = app.selected().unwrap_or(0);
                    let next = if w as u16 == VK_DOWN {
                        (current + 1).min(app.hits.len().saturating_sub(1))
                    } else {
                        current.saturating_sub(1)
                    };
                    app.select(next);
                }
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
                KillTimer(hwnd, ANIMATION_TIMER)
            };
            if !app.font.is_null() {
                unsafe { DeleteObject(app.font) };
            }
            app.hover.rebuild(hwnd, false, false);
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
    if !hover::register() {
        return Err("cannot register the hover zone".into());
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
    unsafe {
        (*app)
            .hover
            .rebuild(hwnd, (*app).settings.hover, (*app).settings.left);
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
