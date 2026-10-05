//! Turns Windows' own search off and on. Closing SearchHost is not enough: Windows
//! starts it again within seconds. The "DisableSearch" policy removes the search UI and
//! its entry points (Win+S, typing in Start, the taskbar box), so SearchHost stays
//! closed; the indexer service is stopped and disabled with it.
//!
//! Turning it off also removes the index the indexer keeps. That is what actually frees
//! the disk space, and it is a cache rather than user data: Windows builds it again by
//! itself if search is ever turned back on.
//!
//! All of it needs an administrator, so the window runs itself elevated with
//! `--windows-search on|off`. The elevated half has no way to talk to the window, so it
//! reports what it did in its exit code and the window reads that back.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::ptr::null_mut;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, REG_DWORD, RRF_RT_REG_DWORD, RegDeleteKeyValueW, RegGetValueW,
    RegSetKeyValueW,
};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
    SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
    SERVICE_CONTROL_STOP, SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DISABLED, SERVICE_NO_CHANGE,
    SERVICE_QUERY_STATUS, SERVICE_START, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP,
    SERVICE_STOPPED, StartServiceW,
};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::wide;

pub const ARGUMENT: &str = "--windows-search";
const POLICY_KEY: &str = r"SOFTWARE\Policies\Microsoft\Windows\Windows Search";
const POLICY_VALUE: &str = "DisableSearch";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Windows' own indexer, and the folder it keeps its index in. The folder is a cache:
/// Windows rebuilds it from the drives whenever search runs again.
const INDEXER_SERVICE: &str = "WSearch";
const INDEXER_DATA: &str = r"C:\ProgramData\Microsoft\Search\Data";
/// How long to wait for the indexer to finish stopping. Its files stay locked until the
/// process is really gone, so the wait decides whether the space can be freed.
const STOP_WAIT: Duration = Duration::from_millis(100);
const STOP_TRIES: usize = 100;

/// Exit codes of the elevated half.
const OFF_AND_CLEARED: u32 = 0;
const OFF_INDEX_REMAINED: u32 = 1;
const ON_RESTORED: u32 = 2;
const ON_NOT_RESTORED: u32 = 3;
const FAILED: u32 = 4;

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

/// What the elevated half did, as the window needs to hear it.
pub struct Outcome {
    /// Windows search is now in the state that was asked for.
    pub changed: bool,
    /// Turning it off: its index files were removed too, which is what frees the space.
    pub index_cleared: bool,
    /// Turning it on: it is set up to run again. Not "already running": Windows starts its
    /// indexer in its own time, and waiting for that would freeze the window for half a
    /// minute to learn something the user does not need to be told.
    pub restored: bool,
}

/// Runs this program elevated to switch Windows search; the user confirms the UAC
/// prompt. Reads the outcome from the elevated copy's exit code, never from a guess.
pub fn request(hwnd: HWND, off: bool) -> Outcome {
    read_code(elevate(hwnd, off), is_off(), off)
}

/// The window's reading of the elevated copy's exit code. Separate from the call itself
/// so the mapping can be checked without a registry or an administrator.
fn read_code(code: u32, now_off: bool, wanted_off: bool) -> Outcome {
    Outcome {
        changed: now_off == wanted_off,
        index_cleared: code == OFF_AND_CLEARED,
        restored: code == ON_RESTORED,
    }
}

fn elevate(hwnd: HWND, off: bool) -> u32 {
    let Ok(exe) = std::env::current_exe() else {
        return FAILED;
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
    // A refused prompt leaves the process handle null, which reads back as `FAILED`.
    if unsafe { ShellExecuteExW(&mut info) } == 0 || info.hProcess.is_null() {
        return FAILED;
    }
    let mut code = FAILED;
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut raw = 0u32;
        if GetExitCodeProcess(info.hProcess, &mut raw) != 0 {
            code = raw;
        }
        CloseHandle(info.hProcess);
    }
    code
}

/// The elevated half of [`request`]. Returns the exit code the window reads back.
pub fn apply(off: bool) -> u32 {
    let key = wide(POLICY_KEY);
    let value = wide(POLICY_VALUE);
    if off {
        let one = 1u32;
        let written = unsafe {
            RegSetKeyValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                REG_DWORD,
                (&raw const one).cast(),
                size_of::<u32>() as u32,
            )
        };
        if written != 0 {
            return FAILED;
        }
        stop_indexer();
        // The search UI is not a service, so the policy only stops it from coming back:
        // this closes the copy that is on screen now.
        run("taskkill.exe", &["/f", "/im", "SearchHost.exe"]);
        if clear_index() {
            OFF_AND_CLEARED
        } else {
            OFF_INDEX_REMAINED
        }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr()) };
        if start_indexer() {
            ON_RESTORED
        } else {
            ON_NOT_RESTORED
        }
    }
}

fn run(program: &str, args: &[&str]) {
    let _ = Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

/// Opens a service with the rights a caller asks for. `None` when it cannot be reached.
fn open_service(access: u32) -> Option<HANDLE> {
    let name = wide(INDEXER_SERVICE);
    // SAFETY: both strings are NUL-terminated; a null machine name means this PC.
    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return None;
        }
        let service = OpenServiceW(manager, name.as_ptr(), access);
        CloseServiceHandle(manager);
        (!service.is_null()).then_some(service)
    }
}

/// Disables the indexer and waits for it to really stop, because its index files stay
/// locked while it runs. Whether it was already stopped does not matter.
fn stop_indexer() {
    let Some(service) = open_service(SERVICE_CHANGE_CONFIG | SERVICE_STOP | SERVICE_QUERY_STATUS)
    else {
        return;
    };
    // SAFETY: the handle is open; nulls leave every setting but the start type alone.
    unsafe {
        ChangeServiceConfigW(
            service,
            SERVICE_NO_CHANGE,
            SERVICE_DISABLED,
            SERVICE_NO_CHANGE,
            std::ptr::null(),
            std::ptr::null(),
            null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        );
        let mut status = SERVICE_STATUS::default();
        // Fails when it was not running, which is not a problem.
        ControlService(service, SERVICE_CONTROL_STOP, &mut status);
    }
    wait_for(service, SERVICE_STOPPED);
    unsafe { CloseServiceHandle(service) };
}

/// Puts the indexer back to starting with Windows (delayed, as Windows ships it) and asks
/// it to start now. It is not waited for: Windows brings its own indexer up in its own
/// time, and the window is blocked on this call while it runs, so a long wait here would
/// freeze the window to learn something that is not worth telling. What is reported is
/// what can be checked at once: the settings that make search work again were restored.
fn start_indexer() -> bool {
    let Some(service) = open_service(SERVICE_CHANGE_CONFIG | SERVICE_START) else {
        return false;
    };
    // SAFETY: the handle is open; nulls leave every setting but the start type alone.
    let configured = unsafe {
        let restored = ChangeServiceConfigW(
            service,
            SERVICE_NO_CHANGE,
            SERVICE_AUTO_START,
            SERVICE_NO_CHANGE,
            std::ptr::null(),
            std::ptr::null(),
            null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        );
        let delayed = SERVICE_DELAYED_AUTO_START_INFO {
            fDelayedAutostart: 1,
        };
        ChangeServiceConfig2W(
            service,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            (&raw const delayed).cast(),
        );
        // Already running is a success too, and is not an error worth reporting.
        StartServiceW(service, 0, std::ptr::null());
        restored
    };
    unsafe { CloseServiceHandle(service) };
    configured != 0
}

/// Waits for a service to reach a state. Stopping is asynchronous, so asking once would
/// read the state from before the request. Used only when stopping: the index files stay
/// locked until the process is really gone, and that is what the wait is for.
fn wait_for(service: HANDLE, wanted: u32) -> bool {
    for _ in 0..STOP_TRIES {
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0u32;
        let size = size_of::<SERVICE_STATUS_PROCESS>() as u32;
        // SAFETY: the buffer is as large as the size passed in, for this info level.
        let ok = unsafe {
            QueryServiceStatusEx(
                service,
                SC_STATUS_PROCESS_INFO,
                (&raw mut status).cast(),
                size,
                &mut needed,
            )
        };
        if ok == 0 {
            return false;
        }
        if status.dwCurrentState == wanted {
            return true;
        }
        std::thread::sleep(STOP_WAIT);
    }
    false
}

/// Removes the files under the indexer's folder, which is what frees the disk space. The
/// folder itself is left in place so its permissions stay as Windows made them.
fn clear_index() -> bool {
    clear_index_at(Path::new(INDEXER_DATA))
}

/// A folder that was never created counts as cleared: there is nothing to free.
fn clear_index_at(root: &Path) -> bool {
    if !root.exists() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    let mut cleared = true;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        let removed = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        cleared &= removed.is_ok();
    }
    cleared
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_that_was_never_there_counts_as_cleared() {
        assert!(clear_index_at(Path::new(r"C:\no\such\index\folder")));
    }

    #[test]
    fn clearing_removes_contents_but_keeps_the_folder() {
        let root = std::env::temp_dir().join("bs-clear-index-test");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("Windows.db"), b"index").unwrap();
        std::fs::write(root.join("nested").join("gather.db"), b"log").unwrap();
        assert!(clear_index_at(&root));
        assert!(root.exists());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_outcome_never_claims_more_than_the_exit_code_says() {
        let cleared = read_code(OFF_AND_CLEARED, true, true);
        assert!(cleared.changed);
        assert!(cleared.index_cleared);
        assert!(!cleared.restored);

        // Turned off, but the index could not be removed: the space was not freed.
        let stuck = read_code(OFF_INDEX_REMAINED, true, true);
        assert!(stuck.changed && !stuck.index_cleared);

        // Turned back on: Windows search is set up to run again.
        let back = read_code(ON_RESTORED, false, false);
        assert!(back.changed && back.restored);

        // Turned on, but Windows would not take its indexer back.
        let refused = read_code(ON_NOT_RESTORED, false, false);
        assert!(refused.changed && !refused.restored);

        // A refused prompt, and nothing changed: no claim of success.
        let dismissed = read_code(FAILED, false, true);
        assert!(!dismissed.changed && !dismissed.index_cleared && !dismissed.restored);
    }
}
