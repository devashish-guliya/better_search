//! How much Windows search is using, so a window can show the two side by side.
//!
//! The service is the only part of better_search that runs with enough rights to read
//! this: Windows keeps its index in a folder that ordinary users cannot open. Reading is
//! all that happens here. Nothing is changed, and a failure to measure is reported as
//! "unknown" rather than as zero, because a number that is wrong is worse than no number.

use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

/// Where Windows keeps the search index. Reading it needs the rights the service has.
const DATA_DIR: &str = r"C:\ProgramData\Microsoft\Search";

/// What Windows search costs right now: its processes' memory, and its index on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    pub memory: Option<u64>,
    pub disk: Option<u64>,
}

pub fn footprint() -> Footprint {
    let dir = Path::new(DATA_DIR);
    Footprint {
        memory: indexer_memory(),
        disk: dir.exists().then(|| directory_size(dir)).flatten(),
    }
}

/// Memory committed by the Windows search indexer processes. `None` when none is running,
/// which is what Windows search being off looks like. The figure is the working set, the
/// one Task Manager's Memory column shows, so the two can be compared by looking at them.
fn indexer_memory() -> Option<u64> {
    let mut total = 0u64;
    let mut found = false;
    // SAFETY: the snapshot handle is checked and closed; the entry struct is a plain
    // C struct whose size is passed in `dwSize` as the API requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let name = String::from_utf16_lossy(
                    &entry.szExeFile[..entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len())],
                );
                if name.eq_ignore_ascii_case("SearchIndexer.exe")
                    && let Some(bytes) = process_memory(entry.th32ProcessID)
                {
                    total = total.saturating_add(bytes);
                    found = true;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    found.then_some(total)
}

fn process_memory(pid: u32) -> Option<u64> {
    // SAFETY: the handle is checked and closed; the struct size matches the type.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let size = counters.cb;
        let ok = K32GetProcessMemoryInfo(process, (&raw mut counters).cast(), size);
        CloseHandle(process);
        (ok != 0).then_some(counters.WorkingSetSize as u64)
    }
}

/// Bytes of every file under `dir`. `None` when the folder cannot be walked, so a caller
/// never shows a partial total as if it were the whole.
fn directory_size(dir: &Path) -> Option<u64> {
    let mut total = 0u64;
    let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let entries = std::fs::read_dir(&next).ok()?;
        for entry in entries {
            let entry = entry.ok()?;
            let kind = entry.file_type().ok()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if let Some(len) = entry.metadata().ok().map(|m| m.len()) {
                total = total.saturating_add(len);
            }
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_folder_is_unknown_rather_than_zero() {
        assert_eq!(directory_size(Path::new(r"C:\no\such\folder\here")), None);
    }

    #[test]
    fn measures_a_folder_it_can_read() {
        let dir = std::env::temp_dir().join("bs-winsearch-size-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), vec![0u8; 1234]).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("b.bin"), vec![0u8; 66]).unwrap();
        assert_eq!(directory_size(&dir), Some(1300));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_indexer_process_is_found_when_it_runs() {
        // Not an assertion about this machine's state: either answer is valid, and a
        // reported number must never be a bogus zero.
        if let Some(bytes) = indexer_memory() {
            assert!(bytes > 0, "an indexer with no memory is not credible");
        }
    }
}
