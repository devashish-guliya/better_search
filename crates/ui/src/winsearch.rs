//! Turns Windows' own search off and on. Closing SearchHost is not enough: Windows
//! starts it again within seconds. The "DisableSearch" policy removes the search UI
//! and its entry points (Win+S, typing in Start, the taskbar box), so SearchHost stays
//! closed; the indexer service is stopped and disabled with it. Both need an
//! administrator, so the window runs itself elevated with `--windows-search on|off`.

use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, REG_DWORD, RRF_RT_REG_DWORD, RegDeleteKeyValueW, RegGetValueW,
    RegSetKeyValueW,
};
use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::wide;

pub const ARGUMENT: &str = "--windows-search";
const POLICY_KEY: &str = r"SOFTWARE\Policies\Microsoft\Windows\Windows Search";
const POLICY_VALUE: &str = "DisableSearch";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether the policy that turns Windows search off is set.
pub fn is_off() -> bool {
    let key = wide(POLICY_KEY);
    let value = wide(POLICY_VALUE);
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut data).cast(),
            &mut size,
        )
    };
    status == 0 && data == 1
}

/// Runs this program elevated to switch Windows search; the user confirms the UAC
/// prompt. Returns whether Windows search is now in the wanted state.
pub fn request(hwnd: HWND, off: bool) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let file = wide(&exe.to_string_lossy());
    let args = wide(&format!("{ARGUMENT} {}", if off { "off" } else { "on" }));
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: args.as_ptr(),
        nShow: SW_HIDE,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return false;
    }
    if !info.hProcess.is_null() {
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            CloseHandle(info.hProcess);
        }
    }
    is_off() == off
}

fn run(program: &str, args: &[&str]) {
    let _ = Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

/// The elevated half of [`request`].
pub fn apply(off: bool) {
    let key = wide(POLICY_KEY);
    let value = wide(POLICY_VALUE);
    if off {
        let one = 1u32;
        unsafe {
            RegSetKeyValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                REG_DWORD,
                (&raw const one).cast(),
                size_of::<u32>() as u32,
            )
        };
        run("sc.exe", &["config", "WSearch", "start=", "disabled"]);
        run("sc.exe", &["stop", "WSearch"]);
        run("taskkill.exe", &["/f", "/im", "SearchHost.exe"]);
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr()) };
        run("sc.exe", &["config", "WSearch", "start=", "delayed-auto"]);
        run("sc.exe", &["start", "WSearch"]);
    }
}
