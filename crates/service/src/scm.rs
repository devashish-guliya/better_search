//! Service Control Manager glue.

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc::{self, Sender};

use windows_sys::Win32::Foundation::{ERROR_CALL_NOT_IMPLEMENTED, NO_ERROR};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STOP_PENDING, SERVICE_STOPPED,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
};

use crate::Stop;

/// Exit code reported to the SCM when the service could not run.
const ERROR_SERVICE_SPECIFIC: u32 = 1066;
/// Compaction plus save take well under a second; this leaves room for a slow disk.
const STOP_WAIT_HINT_MS: u32 = 20_000;

static STATUS: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static STOP: Mutex<Option<Sender<Stop>>> = Mutex::new(None);

/// Hands the main thread to the SCM until the service stops.
pub fn dispatch() -> io::Result<()> {
    // Ignored for a service that runs in its own process, but must not be empty.
    let mut name: Vec<u16> = "better_search".encode_utf16().chain([0]).collect();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: null_mut(),
            lpServiceProc: None,
        },
    ];
    // SAFETY: the table is terminated by a null entry and outlives the call.
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_status(state: SERVICE_STATUS_CURRENT_STATE, exit_code: u32, wait_hint: u32) {
    let handle = STATUS.load(Ordering::Acquire);
    if handle.is_null() {
        return;
    }
    let accepted = if state == SERVICE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
    } else {
        0
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: exit_code,
        dwServiceSpecificExitCode: u32::from(exit_code != NO_ERROR),
        dwCheckPoint: 0,
        dwWaitHint: wait_hint,
    };
    // SAFETY: the handle came from RegisterServiceCtrlHandlerExW and needs no closing.
    unsafe { SetServiceStatus(handle, &status) };
}

unsafe extern "system" fn service_main(_argc: u32, argv: *mut windows_sys::core::PWSTR) {
    let (tx, rx) = mpsc::channel();
    *STOP.lock().unwrap() = Some(tx);
    // SAFETY: the SCM passes the service name as the first argument.
    let name = unsafe { *argv };
    // SAFETY: `name` is a NUL-terminated string from the SCM.
    let handle = unsafe { RegisterServiceCtrlHandlerExW(name, Some(on_control), null()) };
    if handle.is_null() {
        return;
    }
    STATUS.store(handle, Ordering::Release);
    // Loading continues in the background; searches get "loading" until it is done.
    set_status(SERVICE_RUNNING, NO_ERROR, 0);
    let result = crate::run(false, &rx);
    let exit_code = if result.is_ok() {
        NO_ERROR
    } else {
        ERROR_SERVICE_SPECIFIC
    };
    set_status(SERVICE_STOPPED, exit_code, 0);
}

unsafe extern "system" fn on_control(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    let stop = match control {
        SERVICE_CONTROL_STOP => Stop::Save,
        SERVICE_CONTROL_SHUTDOWN => Stop::Fast,
        SERVICE_CONTROL_INTERROGATE => return NO_ERROR,
        _ => return ERROR_CALL_NOT_IMPLEMENTED,
    };
    set_status(SERVICE_STOP_PENDING, NO_ERROR, STOP_WAIT_HINT_MS);
    if let Some(tx) = &*STOP.lock().unwrap() {
        let _ = tx.send(stop);
    }
    NO_ERROR
}
