//! Blocking pipe I/O stays off the window thread. The worker owns one connection for
//! consecutive keystrokes; a failed connection is retried on the next request.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use bs_pipe::{Client, Reply, Status};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, HWND};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const WM_SEARCH_RESULT: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1;
pub const LIMIT: u16 = 200;

pub struct Request {
    pub serial: u64,
    pub text: String,
    pub include_system: bool,
}

pub enum Outcome {
    Reply(Reply),
    Unavailable,
    Denied,
    Error(String),
}

pub struct ResultMessage {
    pub serial: u64,
    pub outcome: Outcome,
}

pub fn start(hwnd: HWND) -> (Sender<Request>, Receiver<ResultMessage>) {
    let (tx, rx) = mpsc::channel();
    let (results, received) = mpsc::channel();
    let target = hwnd as isize;
    thread::Builder::new()
        .name("window-search".into())
        .spawn(move || worker(target, rx, results))
        .expect("cannot start the search worker");
    (tx, received)
}

fn worker(target: isize, rx: Receiver<Request>, results: Sender<ResultMessage>) {
    let mut client = None;
    while let Ok(mut request) = rx.recv() {
        // Only the latest text matters if the user types faster than the service replies.
        loop {
            match rx.try_recv() {
                Ok(next) => request = next,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if client.is_none() {
            match Client::connect() {
                Ok(connected) => client = Some(connected),
                Err(err) => {
                    post(target, &results, request.serial, classify(&err));
                    continue;
                }
            }
        }
        let outcome = match client.as_mut().expect("connected above").search(
            &request.text,
            LIMIT,
            request.include_system,
        ) {
            Ok(reply) if reply.status == Status::Denied => Outcome::Denied,
            Ok(reply) => Outcome::Reply(reply),
            Err(err) => {
                client = None;
                classify(&err)
            }
        };
        post(target, &results, request.serial, outcome);
    }
}

fn classify(err: &io::Error) -> Outcome {
    if err.kind() == io::ErrorKind::PermissionDenied {
        Outcome::Denied
    } else if matches!(
        err.raw_os_error(),
        Some(code) if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_PIPE_BUSY as i32
    ) {
        Outcome::Unavailable
    } else {
        Outcome::Error(err.to_string())
    }
}

fn post(target: isize, results: &Sender<ResultMessage>, serial: u64, outcome: Outcome) {
    if results.send(ResultMessage { serial, outcome }).is_ok() {
        // The receiver owns the result even if the window closes before this message
        // is dispatched. No raw pointer can be stranded in the window's message queue.
        unsafe { PostMessageW(target as HWND, WM_SEARCH_RESULT, 0, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_missing_service_and_access_denial() {
        assert!(matches!(
            classify(&io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32)),
            Outcome::Unavailable
        ));
        assert!(matches!(
            classify(&io::Error::from(io::ErrorKind::PermissionDenied)),
            Outcome::Denied
        ));
        assert!(matches!(
            classify(&io::Error::from(io::ErrorKind::BrokenPipe)),
            Outcome::Error(_)
        ));
    }
}
