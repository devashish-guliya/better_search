//! better_search background service: keeps the index of all fixed NTFS drives live and
//! answers searches over the named pipe `\\.\pipe\better_search`.
//!
//! Runs as LocalSystem under the Service Control Manager, or with `--console` in an
//! elevated terminal for debugging.

mod caller;
mod logfile;
mod removable;
mod scm;
mod security;
mod server;

use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use bs_engine::{Config, Engine, Log, fmt, memory};
use windows_sys::Win32::Foundation::TRUE;
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

use logfile::LogFile;
use server::State;

const USAGE: &str = "\
Usage:
  bs-service             Started by the Service Control Manager.
  bs-service --console   Run in this terminal instead (needs \"Run as administrator\").
                         Press Enter or Ctrl+C to stop; the index is saved first.

Data (snapshot and log): %ProgramData%\\better_search, readable only by SYSTEM and
administrators.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// A normal stop: save the index first.
    Save,
    /// Windows is shutting down: stop at once; the journal replays what was missed.
    Fast,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => match scm::dispatch() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: not started as a service ({e}).\n\n{USAGE}");
                ExitCode::from(2)
            }
        },
        ["--console"] => {
            let (tx, rx) = mpsc::channel();
            watch_console(tx);
            match run(true, &rx) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("error: {msg}");
                    ExitCode::FAILURE
                }
            }
        }
        ["-h" | "--help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("error: unknown arguments\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Runs until a stop request arrives. The pipe answers "loading" until the index is ready.
pub fn run(console: bool, stop: &Receiver<Stop>) -> Result<(), String> {
    let started = Instant::now();
    let dir = bs_engine::machine_data_dir();
    let notes =
        security::secure_dir(&dir).map_err(|e| format!("cannot prepare {}: {e}", dir.display()))?;
    let file = Arc::new(LogFile::open(dir.join("service.log"), console));
    let log: Log = {
        let file = Arc::clone(&file);
        Arc::new(move |line: &str| file.line(line))
    };
    log(&format!(
        "starting better_search service {} (process {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    ));
    for note in notes {
        log(&note);
    }

    let state = Arc::new(State::new(Arc::clone(&log)));
    let removable = Arc::new(
        removable::Removable::start(Arc::clone(&log))
            .map_err(|e| format!("cannot start removable drive monitoring: {e}"))?,
    );
    state.set_removable(&removable);
    if let Err(e) = server::start(Arc::clone(&state)) {
        let msg = format!(
            "cannot create {} ({e}); is the service already running?",
            bs_pipe::PIPE_NAME
        );
        log(&msg);
        return Err(msg);
    }

    let loader = {
        let state = Arc::clone(&state);
        let log = Arc::clone(&log);
        let snapshot = dir.join("index.bin");
        thread::Builder::new()
            .name("load".into())
            .spawn(move || {
                let config = Config {
                    drives: Vec::new(),
                    snapshot,
                    rescan: false,
                    skip_clutter: true,
                };
                match Engine::open(&config, Arc::clone(&log)) {
                    Ok(engine) => {
                        engine.start();
                        let entries = engine.read().live_len();
                        let index_bytes = engine.read().memory_usage().total();
                        let _ = state.engine.set(engine);
                        let private = memory::process_memory().map_or(0, |m| m.private);
                        log(&format!(
                            "ready: {} files and folders in {} · index {} · process private {}",
                            fmt::count(entries),
                            fmt::duration(started.elapsed()),
                            fmt::bytes(index_bytes),
                            fmt::bytes(private)
                        ));
                    }
                    Err(e) => log(&format!("cannot load the index: {e}")),
                }
            })
            .map_err(|e| e.to_string())?
    };

    let kind = stop.recv().unwrap_or(Stop::Save);
    log(&format!("stop requested ({kind:?})"));
    removable.shutdown();
    match state.engine.get() {
        Some(engine) => engine.shutdown(kind == Stop::Save),
        None if loader.is_finished() => {}
        None => log("stopped while the index was still loading; nothing to save"),
    }
    log("stopped");
    Ok(())
}

static CONSOLE_STOP: Mutex<Option<Sender<Stop>>> = Mutex::new(None);

/// Stops on Enter, Ctrl+C or closing the window.
fn watch_console(tx: Sender<Stop>) {
    *CONSOLE_STOP.lock().unwrap() = Some(tx.clone());
    unsafe extern "system" fn on_ctrl(_kind: u32) -> windows_sys::core::BOOL {
        if let Some(tx) = &*CONSOLE_STOP.lock().unwrap() {
            let _ = tx.send(Stop::Save);
        }
        // Give the main thread time to save; Windows ends the process when this returns
        // for a window close, so wait briefly.
        thread::sleep(std::time::Duration::from_secs(2));
        TRUE
    }
    // SAFETY: the handler is a valid function for the life of the process.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), TRUE) };
    thread::spawn(move || {
        let mut line = String::new();
        // End of input (no console attached) is not a stop request.
        if std::io::stdin().read_line(&mut line).unwrap_or(0) > 0 {
            let _ = tx.send(Stop::Save);
        }
    });
}
