//! Transient indexes for removable FAT-family volumes. Nothing here is saved to disk.
//! A notification triggers a replacement scan rather than applying path-based deltas:
//! directory renames and lost notifications cannot leave orphaned or duplicate entries.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bs_engine::Log;
use bs_engine::profiles::Profiles;
use bs_index::{Index, IndexBuilder};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_SYSTEM, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, FindClose,
    FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, GetDriveTypeW,
    GetLogicalDrives, GetVolumeInformationW, OPEN_EXISTING, ReadDirectoryChangesW,
    WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::caller;

const DRIVE_REMOVABLE: u32 = 2;
const CHECK_EVERY: Duration = Duration::from_secs(2);
const RETRY_SCAN: Duration = Duration::from_secs(15);
// Refuse an oversized volume outright rather than reporting misleading partial counts.
const MAX_ENTRIES: usize = 2_000_000;

pub struct VolumeData {
    pub index: RwLock<Index>,
    pub profiles: Mutex<Arc<Profiles>>,
}

impl VolumeData {
    pub(crate) fn new(mut index: Index, folders: &[String]) -> Self {
        index.set_profile_folders(folders);
        let profiles = Arc::new(Profiles::build(&index, index.users_epoch()));
        Self {
            index: RwLock::new(index),
            profiles: Mutex::new(profiles),
        }
    }

    pub(crate) fn set_profile_folders(&self, folders: &[String]) {
        let mut index = self.index.write().unwrap();
        let before = index.users_epoch();
        index.set_profile_folders(folders);
        if index.users_epoch() != before {
            *self.profiles.lock().unwrap() = Arc::new(Profiles::build(&index, index.users_epoch()));
        }
    }
}

type Volumes = Arc<RwLock<BTreeMap<char, Arc<VolumeData>>>>;

pub struct Removable {
    volumes: Volumes,
    stopping: Arc<AtomicBool>,
    manager: Mutex<Option<JoinHandle<()>>>,
}

impl Removable {
    pub fn start(log: Log) -> io::Result<Self> {
        let volumes: Volumes = Arc::default();
        let stopping = Arc::new(AtomicBool::new(false));
        let manager = {
            let volumes = Arc::clone(&volumes);
            let stopping = Arc::clone(&stopping);
            thread::Builder::new()
                .name("removable-drives".into())
                .spawn(move || manage(volumes, stopping, log))?
        };
        Ok(Self {
            volumes,
            stopping,
            manager: Mutex::new(Some(manager)),
        })
    }

    pub fn indexes(&self) -> Vec<Arc<VolumeData>> {
        self.volumes.read().unwrap().values().cloned().collect()
    }

    pub fn set_profile_folders(&self, folders: &[String]) {
        for data in self.indexes() {
            data.set_profile_folders(folders);
        }
    }

    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(manager) = self.manager.lock().unwrap().take() {
            manager.thread().unpark();
            let _ = manager.join();
        }
        self.volumes.write().unwrap().clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Drive {
    letter: char,
    serial: u32,
}

fn detected() -> Vec<Drive> {
    // SAFETY: GetLogicalDrives takes no arguments.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|&n| mask & (1 << n) != 0)
        .filter_map(|n| {
            let letter = char::from(b'A' + n);
            serial(letter).map(|serial| Drive { letter, serial })
        })
        .collect()
}

fn serial(letter: char) -> Option<u32> {
    let root = wide(&format!("{letter}:\\"));
    // SAFETY: `root` is NUL-terminated.
    if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_REMOVABLE {
        return None;
    }
    let mut volume_serial = 0;
    let mut fs = [0u16; 32];
    // SAFETY: output buffers have their declared lengths; unused outputs are null.
    if unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            null_mut(),
            0,
            &mut volume_serial,
            null_mut(),
            null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    } == 0
    {
        return None;
    }
    let len = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    let kind = String::from_utf16_lossy(&fs[..len]);
    matches!(
        kind.to_ascii_uppercase().as_str(),
        "FAT" | "FAT32" | "EXFAT"
    )
    .then_some(volume_serial)
}

fn manage(volumes: Volumes, stopping: Arc<AtomicBool>, log: Log) {
    let mut active: BTreeMap<char, (u32, Arc<AtomicBool>)> = BTreeMap::new();
    while !stopping.load(Ordering::Acquire) {
        let drives = detected();
        for letter in retire(&mut active, &drives, &volumes) {
            log(&format!("{letter}: removable index discarded"));
        }
        for drive in drives {
            if active.contains_key(&drive.letter) {
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = Arc::clone(&cancel);
            let worker_stop = Arc::clone(&stopping);
            let worker_volumes = Arc::clone(&volumes);
            let worker_log = Arc::clone(&log);
            match thread::Builder::new()
                .name(format!("removable-{}", drive.letter))
                .spawn(move || {
                    watch_drive(
                        drive,
                        &worker_volumes,
                        &worker_cancel,
                        &worker_stop,
                        &worker_log,
                    );
                }) {
                Ok(_) => {
                    active.insert(drive.letter, (drive.serial, cancel));
                }
                Err(e) => log(&format!(
                    "{}: removable watcher unavailable: {e}",
                    drive.letter
                )),
            }
        }
        thread::park_timeout(CHECK_EVERY);
    }
    for (_, cancel) in active.values() {
        cancel.store(true, Ordering::Release);
    }
    // Readers finish their current request; workers check cancellation before publishing.
    volumes.write().unwrap().clear();
}

fn retire(
    active: &mut BTreeMap<char, (u32, Arc<AtomicBool>)>,
    drives: &[Drive],
    volumes: &Volumes,
) -> Vec<char> {
    let changed: Vec<char> = active
        .iter()
        .filter(|(letter, (serial, _))| {
            !drives
                .iter()
                .any(|d| d.letter == **letter && d.serial == *serial)
        })
        .map(|(&letter, _)| letter)
        .collect();
    for &letter in &changed {
        if let Some((_, cancel)) = active.remove(&letter) {
            cancel.store(true, Ordering::Release);
        }
        volumes.write().unwrap().remove(&letter);
    }
    changed
}

fn cancelled(cancel: &AtomicBool, stopping: &AtomicBool) -> bool {
    cancel.load(Ordering::Acquire) || stopping.load(Ordering::Acquire)
}

fn watch_drive(
    drive: Drive,
    volumes: &Volumes,
    cancel: &AtomicBool,
    stopping: &AtomicBool,
    log: &Log,
) {
    let root = format!("{}:\\", drive.letter);
    let mut watcher = match Watcher::open(&root) {
        Ok(mut watcher) => match watcher.arm() {
            Ok(()) => Some(watcher),
            Err(e) => {
                log(&format!(
                    "{}: notification unavailable ({e}); polling",
                    drive.letter
                ));
                None
            }
        },
        Err(e) => {
            log(&format!(
                "{}: notification unavailable ({e}); polling",
                drive.letter
            ));
            None
        }
    };
    loop {
        if cancelled(cancel, stopping) || serial(drive.letter) != Some(drive.serial) {
            return;
        }
        match scan(Path::new(&root), &root, || cancelled(cancel, stopping)) {
            Ok(index)
                if !cancelled(cancel, stopping) && serial(drive.letter) == Some(drive.serial) =>
            {
                let count = index.live_len() - 1;
                let data = Arc::new(VolumeData::new(index, &caller::profile_folders()));
                // The manager may discard the drive concurrently. Its removal holds the
                // write lock after setting `cancel`, so it wins over any earlier insert.
                let mut map = volumes.write().unwrap();
                if cancelled(cancel, stopping) || serial(drive.letter) != Some(drive.serial) {
                    return;
                }
                map.insert(drive.letter, data);
                log(&format!(
                    "{}: indexed {count} removable entries",
                    drive.letter
                ));
            }
            Ok(_) => return,
            Err(_) if cancelled(cancel, stopping) => return,
            Err(e) => {
                // A failed replacement scan must not leave old, inaccurate results.
                volumes.write().unwrap().remove(&drive.letter);
                log(&format!(
                    "{}: removable scan unavailable: {e}",
                    drive.letter
                ));
            }
        }
        let Some(ref mut watch) = watcher else {
            for _ in 0..(RETRY_SCAN.as_secs() * 4) {
                if cancelled(cancel, stopping) {
                    return;
                }
                thread::sleep(Duration::from_millis(250));
            }
            continue;
        };
        loop {
            if cancelled(cancel, stopping) {
                return;
            }
            match watch.wait(1000) {
                Ok(false) => continue,
                Ok(true) => {
                    // Re-arm before scanning so changes during the scan signal again.
                    if let Err(e) = watch.arm() {
                        log(&format!(
                            "{}: notification lost ({e}); polling",
                            drive.letter
                        ));
                        watcher = None;
                    }
                    break;
                }
                Err(e) => {
                    log(&format!(
                        "{}: notification lost ({e}); polling",
                        drive.letter
                    ));
                    watcher = None;
                    break;
                }
            }
        }
    }
}

/// Build beside the published index; on any error, keep no partial results.
fn scan(root: &Path, label: &str, stop: impl Fn() -> bool) -> io::Result<Index> {
    let mut builder = IndexBuilder::new();
    builder.begin_volume(label.trim_end_matches('\\'), 1);
    let mut next_id = 2u64;
    let mut stack = vec![(root.to_path_buf(), 1u64)];
    while let Some((path, parent)) = stack.pop() {
        if stop() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "scan stopped"));
        }
        let pattern = path.join("*");
        let wide_pattern: Vec<u16> = pattern.as_os_str().encode_wide().chain([0]).collect();
        let mut item = WIN32_FIND_DATAW::default();
        // SAFETY: pattern is NUL-terminated and `item` receives a WIN32_FIND_DATAW.
        let mut handle = unsafe {
            FindFirstFileExW(
                wide_pattern.as_ptr(),
                FindExInfoBasic,
                (&raw mut item).cast(),
                FindExSearchNameMatch,
                null(),
                FIND_FIRST_EX_LARGE_FETCH,
            )
        };
        if handle == INVALID_HANDLE_VALUE
            && io::Error::last_os_error().raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER as i32)
        {
            // Older FAT drivers may not support large-fetch enumeration.
            handle = unsafe {
                FindFirstFileExW(
                    wide_pattern.as_ptr(),
                    FindExInfoBasic,
                    (&raw mut item).cast(),
                    FindExSearchNameMatch,
                    null(),
                    0,
                )
            };
        }
        if handle == INVALID_HANDLE_VALUE {
            let e = io::Error::last_os_error();
            if matches!(
                e.raw_os_error(),
                Some(code) if code == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND as i32
                    || code == windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES as i32
            ) {
                continue; // An empty directory.
            }
            return Err(e);
        }
        let _handle = FindHandle(handle);
        loop {
            if stop() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "scan stopped"));
            }
            let attrs = item.dwFileAttributes;
            let len = item
                .cFileName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(item.cFileName.len());
            let name = &item.cFileName[..len];
            if name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16] {
                // File-system enumerators normally suppress these, but not all do.
            } else if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                // A reparse point may escape the volume or create a directory cycle.
                if next_id as usize > MAX_ENTRIES {
                    return Err(io::Error::new(
                        io::ErrorKind::OutOfMemory,
                        "removable volume exceeds entry limit",
                    ));
                }
                let id = next_id;
                next_id += 1;
                let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                builder.push_utf16(
                    id,
                    parent,
                    name,
                    is_dir,
                    attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0,
                );
                if is_dir {
                    stack.push((path.join(OsString::from_wide(name)), id));
                }
            }
            // SAFETY: handle stays open; `item` is valid for the next entry.
            if unsafe { FindNextFileW(handle, &mut item) } == 0 {
                if io::Error::last_os_error().raw_os_error()
                    != Some(windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES as i32)
                {
                    return Err(io::Error::last_os_error());
                }
                break;
            }
        }
    }
    builder.end_volume();
    Ok(builder.finish())
}

struct FindHandle(HANDLE);
impl Drop for FindHandle {
    fn drop(&mut self) {
        // SAFETY: handle came from FindFirstFileExW.
        unsafe { FindClose(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

struct Watcher {
    dir: HANDLE,
    event: HANDLE,
    operation: OVERLAPPED,
    buffer: Vec<u8>,
    pending: bool,
}

impl Watcher {
    fn open(root: &str) -> io::Result<Self> {
        let path = wide(root);
        // SAFETY: the path is NUL-terminated; no security attributes or template.
        let dir = unsafe {
            CreateFileW(
                path.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if dir == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: unnamed manual-reset event; no security attributes.
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            let error = io::Error::last_os_error();
            unsafe { CloseHandle(dir) };
            return Err(error);
        }
        let operation = OVERLAPPED {
            hEvent: event,
            ..OVERLAPPED::default()
        };
        Ok(Self {
            dir,
            event,
            operation,
            buffer: vec![0; 64 * 1024],
            pending: false,
        })
    }

    fn arm(&mut self) -> io::Result<()> {
        // SAFETY: no request is pending; the buffer and OVERLAPPED live until completion.
        unsafe { windows_sys::Win32::System::Threading::ResetEvent(self.event) };
        self.operation = OVERLAPPED {
            hEvent: self.event,
            ..OVERLAPPED::default()
        };
        let ok = unsafe {
            ReadDirectoryChangesW(
                self.dir,
                self.buffer.as_mut_ptr().cast(),
                self.buffer.len() as u32,
                1,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_ATTRIBUTES,
                null_mut(),
                &mut self.operation,
                None,
            )
        };
        if ok == 0 && io::Error::last_os_error().raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            return Err(io::Error::last_os_error());
        }
        self.pending = true;
        Ok(())
    }

    fn wait(&mut self, ms: u32) -> io::Result<bool> {
        // SAFETY: event is owned by self and held until the pending request completes.
        match unsafe { WaitForSingleObject(self.event, ms) } {
            WAIT_TIMEOUT => Ok(false),
            WAIT_OBJECT_0 => {
                let mut bytes = 0;
                // SAFETY: the event signaled for this operation; the buffer remains live.
                let ok = unsafe { GetOverlappedResult(self.dir, &self.operation, &mut bytes, 0) };
                self.pending = false;
                if ok == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(true)
                }
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if self.pending {
            // SAFETY: cancel and wait for the request before freeing its buffer.
            unsafe {
                CancelIoEx(self.dir, &self.operation);
                let mut bytes = 0;
                GetOverlappedResult(self.dir, &self.operation, &mut bytes, 1);
            }
        }
        unsafe {
            CloseHandle(self.event);
            CloseHandle(self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bs_query::{Query, search};

    #[test]
    fn fixture_scan_rename_delete_and_no_partial_results() {
        let root =
            std::env::temp_dir().join(format!("bs-removable-fixture-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        std::fs::create_dir(root.join("Folder")).unwrap();
        std::fs::write(root.join("Folder").join("needle.txt"), b"fixture").unwrap();
        let query = Query::parse("needle").unwrap();
        let first = scan(&root, "R:\\", || false).unwrap();
        assert_eq!(search(&first, &query, 10).total_matches, 1);
        assert_eq!(
            first.full_path(search(&first, &query, 10).hits[0].entry),
            r"R:\Folder\needle.txt"
        );

        std::fs::rename(root.join("Folder"), root.join("Renamed")).unwrap();
        let renamed = scan(&root, "R:\\", || false).unwrap();
        assert_eq!(
            renamed.full_path(search(&renamed, &query, 10).hits[0].entry),
            r"R:\Renamed\needle.txt"
        );
        assert!(scan(&root, "R:\\", || true).is_err());
        std::fs::remove_file(root.join("Renamed").join("needle.txt")).unwrap();
        assert_eq!(
            search(&scan(&root, "R:\\", || false).unwrap(), &query, 10).total_matches,
            0
        );
    }

    #[test]
    fn fixture_notification_and_volume_removal() {
        let root = std::env::temp_dir().join(format!("bs-removable-watch-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let mut watch = Watcher::open(root.to_str().unwrap()).unwrap();
        watch.arm().unwrap();
        std::fs::write(root.join("new.txt"), b"fixture").unwrap();
        assert!(watch.wait(3000).unwrap());
        watch.arm().unwrap();
        std::fs::rename(root.join("new.txt"), root.join("renamed.txt")).unwrap();
        assert!(watch.wait(3000).unwrap());

        let volumes: Volumes = Arc::default();
        let index = scan(&root, "R:\\", || false).unwrap();
        volumes
            .write()
            .unwrap()
            .insert('R', Arc::new(VolumeData::new(index, &[])));
        let cancel = Arc::new(AtomicBool::new(false));
        let mut active = BTreeMap::from([('R', (10, Arc::clone(&cancel)))]);
        assert!(
            retire(
                &mut active,
                &[Drive {
                    letter: 'R',
                    serial: 10
                }],
                &volumes
            )
            .is_empty()
        );
        assert_eq!(
            retire(
                &mut active,
                &[Drive {
                    letter: 'R',
                    serial: 11
                }],
                &volumes
            ),
            vec!['R']
        );
        assert!(cancel.load(Ordering::Acquire));
        assert!(volumes.read().unwrap().is_empty());
    }
}
