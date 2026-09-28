//! A four-pixel, nearly transparent topmost window on each monitor edge. It never
//! activates; only a deliberate 280 ms hover asks the main window to slide in.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetCursorPos,
    GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IDC_ARROW, KillTimer, LWA_ALPHA, LoadCursorW,
    PostMessageW, RegisterClassW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes,
    SetTimer, SetWindowLongPtrW, SetWindowPos, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_TIMER,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::{scale, wide};

pub const WM_HOVER_TRIGGER: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 3;
const CLASS: &str = "BetterSearchHoverZone";
const TIMER: usize = 1;

#[derive(Clone, Copy)]
struct Zone {
    main: HWND,
}

pub struct Hover {
    zones: Vec<HWND>,
}

impl Hover {
    pub fn new() -> Self {
        Self { zones: Vec::new() }
    }

    pub fn rebuild(&mut self, main: HWND, enabled: bool, left: bool) {
        for window in self.zones.drain(..) {
            unsafe { DestroyWindow(window) };
        }
        if !enabled {
            return;
        }
        let mut build = Build {
            main,
            left,
            zones: Vec::new(),
        };
        unsafe {
            EnumDisplayMonitors(
                null_mut(),
                null(),
                Some(add_monitor),
                (&mut build as *mut Build) as _,
            )
        };
        self.zones = build.zones;
    }
}

struct Build {
    main: HWND,
    left: bool,
    zones: Vec<HWND>,
}

unsafe extern "system" fn add_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> i32 {
    let build = unsafe { &mut *(data as *mut Build) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return 1;
    }
    let zone = Zone { main: build.main };
    let x = if build.left {
        info.rcMonitor.left
    } else {
        info.rcMonitor.right - 4
    };
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            wide(CLASS).as_ptr(),
            null(),
            WS_POPUP,
            x,
            info.rcWork.top,
            4,
            info.rcWork.bottom - info.rcWork.top,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            (&raw const zone).cast(),
        )
    };
    if !window.is_null() {
        let width = scale(window, 4);
        let x = if build.left {
            info.rcMonitor.left
        } else {
            info.rcMonitor.right - width
        };
        unsafe {
            SetLayeredWindowAttributes(window, 0, 1, LWA_ALPHA);
            SetWindowPos(
                window,
                HWND_TOPMOST,
                x,
                info.rcWork.top,
                width,
                info.rcWork.bottom - info.rcWork.top,
                SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
        }
        build.zones.push(window);
    }
    1
}

unsafe extern "system" fn zone_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = unsafe { &*(l as *const CREATESTRUCTW) };
        let zone = unsafe { *(create.lpCreateParams as *const Zone) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(Box::new(zone)) as isize) };
        return 1;
    }
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Zone };
    match msg {
        WM_MOUSEMOVE => {
            unsafe { SetTimer(hwnd, TIMER, 280, None) };
            0
        }
        WM_TIMER if w == TIMER && !ptr.is_null() => {
            unsafe { KillTimer(hwnd, TIMER) };
            let mut mouse = POINT::default();
            let mut rect = RECT::default();
            unsafe {
                GetCursorPos(&mut mouse);
                GetWindowRect(hwnd, &mut rect)
            };
            if mouse.x >= rect.left
                && mouse.x < rect.right
                && mouse.y >= rect.top
                && mouse.y < rect.bottom
            {
                let zone = unsafe { &*ptr };
                unsafe { PostMessageW(zone.main, WM_HOVER_TRIGGER, 0, 0) };
            }
            0
        }
        WM_NCDESTROY => {
            if !ptr.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(ptr))
                };
            }
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

pub fn register() -> bool {
    let name = wide(CLASS);
    let class = WNDCLASSW {
        lpfnWndProc: Some(zone_proc),
        hInstance: unsafe { GetModuleHandleW(null()) },
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        lpszClassName: name.as_ptr(),
        ..Default::default()
    };
    unsafe { RegisterClassW(&class) != 0 }
}
