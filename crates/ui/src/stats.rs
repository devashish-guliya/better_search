//! Read-only diagnostics poll on their own worker, never on the UI thread.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use bs_pipe::{Client, StatsReply, Status};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindowVisible, PostMessageW, WM_APP};

pub const WM_STATS_RESULT: u32 = WM_APP + 4;

pub enum Reading {
    Service(StatsReply),
    Unavailable,
    Unsupported,
    Denied,
}

pub fn start(hwnd: HWND) -> (Receiver<Reading>, Sender<()>) {
    let (tx, rx) = mpsc::channel();
    let (stop, until_stop) = mpsc::channel();
    let target = hwnd as isize;
    thread::Builder::new()
        .name("window-stats".into())
        .spawn(move || {
            let mut client = None;
            loop {
                // Hidden panels need no diagnostics traffic or wakeful service work.
                if unsafe { IsWindowVisible(target as HWND) } == 0 {
                    if until_stop.recv_timeout(Duration::from_secs(2))
                        != Err(RecvTimeoutError::Timeout)
                    {
                        break;
                    }
                    continue;
                }
                let denied = if client.is_none() {
                    match Client::connect() {
                        Ok(connected) => {
                            client = Some(connected);
                            false
                        }
                        Err(err) => err.kind() == std::io::ErrorKind::PermissionDenied,
                    }
                } else {
                    false
                };
                let reading = if denied {
                    Reading::Denied
                } else if let Some(connected) = &mut client {
                    match connected.stats() {
                        Ok(reply) if reply.status == Status::Denied => Reading::Denied,
                        Ok(reply) => Reading::Service(reply),
                        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                            Reading::Unsupported
                        }
                        Err(_) => {
                            client = None;
                            Reading::Unavailable
                        }
                    }
                } else {
                    Reading::Unavailable
                };
                if tx.send(reading).is_err() {
                    break;
                }
                // No pointers are sent through a window message. The receiver owns data.
                unsafe { PostMessageW(target as HWND, WM_STATS_RESULT, 0, 0) };
                if until_stop.recv_timeout(Duration::from_secs(2)) != Err(RecvTimeoutError::Timeout)
                {
                    break;
                }
            }
        })
        .expect("cannot start the stats worker");
    (rx, stop)
}

#[derive(Clone, Copy)]
pub struct WindowMemory {
    pub private: u64,
    pub working_set: u64,
}

pub fn window_memory() -> Option<WindowMemory> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo handle needs no closing; the buffer size matches the struct.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    (ok != 0).then_some(WindowMemory {
        private: counters.PrivateUsage as u64,
        working_set: counters.WorkingSetSize as u64,
    })
}

pub fn size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.0} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_small_and_large_resources() {
        assert_eq!(size(2048), "2 KiB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MiB");
        assert!(window_memory().is_some());
    }
}
