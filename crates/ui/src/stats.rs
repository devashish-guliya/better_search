//! One read-only sizes request, on its own thread, so the panel never waits on the pipe.
//!
//! The offer to replace Windows search quotes numbers, so it waits for the first scan to
//! finish: a figure taken while the index is still being built would be wrong. The wait is
//! bounded, because a machine where the service never becomes ready still deserves the
//! offer, just without the numbers.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use bs_pipe::{Client, StatsReply, Status};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindowVisible, PostMessageW, WM_APP};

pub const WM_SIZES_RESULT: u32 = WM_APP + 10;

const RETRY_EVERY: Duration = Duration::from_secs(3);
/// Attempts before giving up on the first scan: three minutes of a service still working.
const ATTEMPTS: usize = 60;

/// What the service said about its own cost.
pub enum Reading {
    /// The service answered, and the first scan is done: these numbers are real.
    Sizes(StatsReply),
    /// The service is still scanning the drives.
    Scanning,
    /// No answer: not running, not ours, or too old to know about this question.
    Unavailable,
}

/// Asks for the sizes and keeps asking while the service is still scanning. The receiver
/// gets every reading, so a panel can show what it has and wait for better.
pub fn start(hwnd: HWND) -> (Receiver<Reading>, Sender<()>) {
    let (tx, rx) = mpsc::channel();
    let (stop, until_stop) = mpsc::channel();
    let target = hwnd as isize;
    thread::Builder::new()
        .name("window-sizes".into())
        .spawn(move || {
            let mut client = None;
            let mut attempts = 0;
            loop {
                // A hidden panel asks nothing: the service has better things to do.
                if unsafe { IsWindowVisible(target as HWND) } == 0 {
                    if until_stop.recv_timeout(RETRY_EVERY) != Err(RecvTimeoutError::Timeout) {
                        return;
                    }
                    continue;
                }
                attempts += 1;
                let last = attempts >= ATTEMPTS;
                let reading = ask(&mut client);
                let done = last || !matches!(reading, Reading::Scanning);
                if tx.send(reading).is_err() {
                    return;
                }
                // No pointer travels through a window message: the receiver owns the data.
                unsafe { PostMessageW(target as HWND, WM_SIZES_RESULT, 0, 0) };
                if done || until_stop.recv_timeout(RETRY_EVERY) != Err(RecvTimeoutError::Timeout) {
                    return;
                }
            }
        })
        .expect("cannot start the sizes worker");
    (rx, stop)
}

fn ask(client: &mut Option<Client>) -> Reading {
    if client.is_none() {
        match Client::connect() {
            Ok(connected) => *client = Some(connected),
            // Not running, or a pipe that is not the service's: either way, no numbers.
            Err(_) => return Reading::Unavailable,
        }
    }
    match client.as_mut().expect("connected above").stats() {
        Ok(reply) => match reply.status {
            Status::Ok => Reading::Sizes(reply),
            Status::Loading => Reading::Scanning,
            // A service from before this question existed answers BadRequest.
            Status::Denied | Status::BadRequest => Reading::Unavailable,
        },
        Err(_) => {
            *client = None;
            Reading::Unavailable
        }
    }
}

/// A size as a person reads it: whole megabytes, because a prompt is not a measurement
/// report, and never "0 MB" for something that exists.
pub fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if (bytes as f64) < MB {
        let kb = (bytes as f64 / 1024.0).round().max(1.0) as u64;
        return format!("{kb} KB");
    }
    let mb = (bytes as f64 / MB).round().max(1.0) as u64;
    format!("{mb} MB")
}

/// A count with thousands separated, as people write numbers.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_whole_megabytes_and_never_as_zero() {
        assert_eq!(size(0), "1 KB");
        assert_eq!(size(1024), "1 KB");
        assert_eq!(size(1024 * 1024), "1 MB");
        assert_eq!(size(19 * 1024 * 1024), "19 MB");
        assert_eq!(size(34_326_528), "33 MB");
        // Rounds up rather than down, so a small cost is never shown as nothing.
        assert_eq!(size(1024 * 1024 + 1024), "1 MB");
        assert_eq!(size(1024 * 1024 * 3 / 2), "2 MB");
    }

    #[test]
    fn counts_are_separated_in_thousands() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(574_455), "574,455");
        assert_eq!(count(1_000_000), "1,000,000");
    }
}
