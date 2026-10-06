//! While Windows search is turned off, a `SearchHost.exe` window that still comes to
//! the front is ended at once. The panel no longer hooks the keyboard: it opens from
//! its hotkey and the tray only.

use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess,
};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_SYSTEM_FOREGROUND, GetMessageW, GetWindowThreadProcessId, MSG, WINEVENT_OUTOFCONTEXT,
};

/// Windows search is turned off, so a SearchHost window that still comes to the
/// front is closed.
static SEARCH_OFF: AtomicBool = AtomicBool::new(false);

/// Installs the foreground watcher on a thread of its own. A WinEvent hook needs a
/// message loop, and it must never wait behind the window's own work.
pub fn install(search_off: bool) {
    SEARCH_OFF.store(search_off, Ordering::Relaxed);
    let _ = std::thread::Builder::new()
        .name("window-watch".into())
        .spawn(|| {
            let _: HWINEVENTHOOK = unsafe {
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    null_mut(),
                    Some(foreground_changed),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                )
            };
            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {}
        });
}

pub fn set_search_off(off: bool) {
    SEARCH_OFF.store(off, Ordering::Relaxed);
}

/// The file name of the program that owns `window`, lowercased.
fn program_of(window: HWND, access: u32) -> Option<(String, isize)> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | access, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut buffer = [0u16; 512];
    let mut len = buffer.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut len)
    };
    let path = String::from_utf16_lossy(&buffer[..len as usize]);
    let name = path.rsplit('\\').next().unwrap_or_default().to_lowercase();
    (ok != 0).then_some((name, process as isize)).or_else(|| {
        unsafe { CloseHandle(process) };
        None
    })
}

unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    window: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if !SEARCH_OFF.load(Ordering::Relaxed) {
        return;
    }
    let Some((name, process)) = program_of(window, PROCESS_TERMINATE) else {
        return;
    };
    if name == "searchhost.exe" {
        unsafe { TerminateProcess(process as _, 0) };
    }
    unsafe { CloseHandle(process as _) };
}
