//! A small log file that keeps at most two generations of about 1 MB.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

const MAX_BYTES: u64 = 1 << 20;

pub struct LogFile {
    path: PathBuf,
    state: Mutex<State>,
    echo: bool,
}

struct State {
    file: Option<File>,
    size: u64,
}

impl LogFile {
    /// With `echo`, lines are printed too (console mode).
    pub fn open(path: PathBuf, echo: bool) -> Self {
        let file = open_append(&path);
        let size = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        Self {
            path,
            state: Mutex::new(State { file, size }),
            echo,
        }
    }

    pub fn line(&self, text: &str) {
        let line = format!("{} {text}\r\n", timestamp());
        if self.echo {
            print!("{line}");
        }
        let mut state = self.state.lock().unwrap();
        if state.size + line.len() as u64 > MAX_BYTES {
            state.file = None;
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.old"));
            state.file = open_append(&self.path);
            state.size = 0;
        }
        if let Some(file) = &mut state.file
            && file.write_all(line.as_bytes()).is_ok()
        {
            state.size += line.len() as u64;
        }
    }
}

fn open_append(path: &PathBuf) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

fn timestamp() -> String {
    // SAFETY: zero is a valid SYSTEMTIME; GetLocalTime fills it.
    let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut t) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}
