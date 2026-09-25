//! Keeps an index of NTFS drives current: loads the saved snapshot (or scans), catches
//! up on changes made while nothing was running, then follows each volume's change
//! journal on its own thread, saves now and then, and lets the index leave RAM when
//! nobody searches.
//!
//! Nothing here prints. Progress and events go to a [`Log`] callback, so the same
//! engine runs in the console tool and in the background service.

pub mod fmt;
pub mod memory;
pub mod profiles;

use std::collections::VecDeque;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bs_index::{Applied, Change, Index, IndexBuilder, SKIP_RULES_VERSION, SyncPoint};
use bs_ntfs::{ChangeKind, JournalInfo, Record, Volume};
use windows_sys::Win32::System::IO::CancelSynchronousIo;

/// Receives one line per progress message or event.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

const LOG_LINES: usize = 50;
/// Searches always use the live in-memory index; the saved copy only makes the next
/// start fast, and changes missed since the last save are replayed from the journal.
/// So saving rarely loses nothing and avoids rewriting the file all day.
const SAVE_EVERY: Duration = Duration::from_secs(60 * 60);
/// After this long without a search, the index is allowed to leave RAM.
const IDLE_TRIM_AFTER: Duration = Duration::from_secs(60);
/// Same size Windows uses for the journal it creates on the system drive.
const JOURNAL_MAX_BYTES: u64 = 32 << 20;
const JOURNAL_GROW_BYTES: u64 = 8 << 20;
/// How long [`Engine::shutdown`] keeps trying to wake a watcher blocked in the kernel.
const STOP_WAIT: Duration = Duration::from_secs(3);

/// `%LOCALAPPDATA%\better_search\index.bin`: the snapshot of the console tool, which
/// runs as the current user.
pub fn user_snapshot_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("better_search").join("index.bin")
}

/// `%ProgramData%\better_search`: the service's folder for its snapshot and log.
pub fn machine_data_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("better_search")
}

pub struct Config {
    /// Drive letters to index; empty means all fixed NTFS drives.
    pub drives: Vec<char>,
    pub snapshot: PathBuf,
    /// Ignore the snapshot and read the drives again.
    pub rescan: bool,
    pub skip_clutter: bool,
}

/// State shared with the background threads.
struct Shared {
    index: RwLock<Index>,
    log: Log,
    /// Latest changes and events, for display.
    recent: Mutex<VecDeque<String>>,
    saved_generation: Mutex<Option<u64>>,
    /// Held by whoever changes or saves the index. Writers take it before the write
    /// lock, so while a compaction or save holds a read lock for a while, no writer is
    /// queued on the RwLock, and a queued writer would make new searches wait.
    maintenance: Mutex<()>,
    last_search: Mutex<Instant>,
    trimmed: AtomicBool,
    stop: AtomicBool,
    snapshot: Option<PathBuf>,
}

impl Shared {
    fn push_recent(&self, line: String) {
        let mut recent = self.recent.lock().unwrap();
        if recent.len() == LOG_LINES {
            recent.pop_front();
        }
        recent.push_back(line);
    }

    /// An event worth keeping in the log, as opposed to a single file change.
    fn event(&self, line: String) {
        (self.log)(&line);
        self.push_recent(line);
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct Workers {
    watchers: Vec<JoinHandle<()>>,
    saver: Option<JoinHandle<()>>,
}

/// An index plus the machinery that keeps it current.
pub struct Engine {
    shared: Arc<Shared>,
    workers: Mutex<Workers>,
}

impl Engine {
    /// Loads or builds the index of the configured NTFS drives and brings it fully up
    /// to date. Call [`Self::start`] to keep it current afterwards.
    pub fn open(config: &Config, log: Log) -> Result<Self, String> {
        let letters = if config.drives.is_empty() {
            bs_ntfs::fixed_ntfs_volumes()
        } else {
            config.drives.clone()
        };
        if letters.is_empty() {
            return Err("no fixed NTFS drives found".into());
        }
        let (index, saved) = load_or_scan(&letters, config, &*log)?;
        let engine = Self::new(index, log, Some(config.snapshot.clone()));
        if saved {
            let generation = engine.read().generation();
            *engine.shared.saved_generation.lock().unwrap() = Some(generation);
        }
        Ok(engine)
    }

    /// Wraps an index that is not backed by drives (a walked folder, test data): no
    /// live updates and no snapshot.
    pub fn from_index(index: Index, log: Log) -> Self {
        Self::new(index, log, None)
    }

    fn new(index: Index, log: Log, snapshot: Option<PathBuf>) -> Self {
        Self {
            shared: Arc::new(Shared {
                index: RwLock::new(index),
                log,
                recent: Mutex::new(VecDeque::new()),
                saved_generation: Mutex::new(None),
                maintenance: Mutex::new(()),
                last_search: Mutex::new(Instant::now()),
                trimmed: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                snapshot,
            }),
            workers: Mutex::new(Workers::default()),
        }
    }

    /// True when the index follows change journals and is saved to a snapshot.
    pub fn is_live(&self) -> bool {
        self.shared.snapshot.is_some()
    }

    /// Starts one watcher thread per volume with a change journal, plus the thread that
    /// saves and trims memory. Does nothing for an index without drives.
    pub fn start(&self) {
        if !self.is_live() {
            return;
        }
        let mut workers = self.workers.lock().unwrap();
        if workers.saver.is_some() {
            return;
        }
        let volumes: Vec<(usize, String, SyncPoint)> = self
            .read()
            .volumes()
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.sync.map(|s| (i, v.label.clone(), s)))
            .collect();
        for (vol, label, sync) in volumes {
            let shared = Arc::clone(&self.shared);
            let handle = thread::Builder::new()
                .name(format!("watch {label}"))
                .spawn(move || watch(&shared, vol, &label, sync))
                .expect("spawning a thread");
            workers.watchers.push(handle);
        }
        let shared = Arc::clone(&self.shared);
        let saver = thread::Builder::new()
            .name("saver".into())
            .spawn(move || run_saver(&shared))
            .expect("spawning a thread");
        workers.saver = Some(saver);
    }

    /// Read access for searching. Searches run in parallel; changes wait meanwhile.
    pub fn read(&self) -> RwLockReadGuard<'_, Index> {
        self.shared.index.read().unwrap()
    }

    /// Records that the user searched, which keeps the index in RAM for a while.
    pub fn touch(&self) {
        *self.shared.last_search.lock().unwrap() = Instant::now();
        self.shared.trimmed.store(false, Ordering::Relaxed);
    }

    /// The latest file changes and events, oldest first.
    pub fn recent_changes(&self) -> Vec<String> {
        self.shared.recent.lock().unwrap().iter().cloned().collect()
    }

    /// Saves now if anything changed since the last save.
    pub fn save(&self) {
        save_shared(&self.shared);
    }

    /// Stops following the journals and, with `save`, writes the snapshot once the
    /// watchers are done. The index stays readable.
    pub fn shutdown(&self, save: bool) {
        let workers = std::mem::take(&mut *self.workers.lock().unwrap());
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(saver) = &workers.saver {
            saver.thread().unpark();
        }
        let deadline = Instant::now() + STOP_WAIT;
        for watcher in workers.watchers {
            // The watcher may be blocked waiting for journal records. Cancelling is
            // repeated because it can land just before the thread enters the wait.
            while !watcher.is_finished() && Instant::now() < deadline {
                // SAFETY: the handle belongs to a live JoinHandle and has full access.
                unsafe { CancelSynchronousIo(watcher.as_raw_handle()) };
                thread::sleep(Duration::from_millis(5));
            }
            if watcher.is_finished() {
                let _ = watcher.join();
            } else {
                (self.shared.log)("a drive watcher did not stop in time");
            }
        }
        if let Some(saver) = workers.saver {
            let _ = saver.join();
        }
        if save {
            self.save();
        }
    }
}

struct OpenVolume {
    letter: char,
    volume: Volume,
    serial: u32,
    journal: Option<JournalInfo>,
}

fn open_volumes(letters: &[char], log: &dyn Fn(&str)) -> Result<Vec<OpenVolume>, String> {
    letters
        .iter()
        .map(|&letter| {
            let volume = Volume::open(letter).map_err(|e| {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    format!(
                        "cannot open drive {letter}: without administrator rights.\n\
                         Open a terminal with \"Run as administrator\" and try again,\n\
                         or use --walk <FOLDER> to test without admin."
                    )
                } else {
                    format!("cannot open drive {letter}: {e}")
                }
            })?;
            let serial =
                bs_ntfs::volume_serial(letter).map_err(|e| format!("drive {letter}: {e}"))?;
            let journal = volume.journal().or_else(|| {
                match bs_ntfs::create_journal(letter, JOURNAL_MAX_BYTES, JOURNAL_GROW_BYTES) {
                    Ok(()) => {
                        log(&format!("Turned on the change journal of drive {letter}:"));
                        volume.journal()
                    }
                    Err(e) => {
                        log(&format!(
                            "Could not turn on the change journal of drive {letter}: ({e})"
                        ));
                        None
                    }
                }
            });
            Ok(OpenVolume {
                letter,
                volume,
                serial,
                journal,
            })
        })
        .collect()
}

/// Loads or builds the index and brings it fully up to date. The flag tells whether
/// the returned index is already saved to disk.
fn load_or_scan(
    letters: &[char],
    config: &Config,
    log: &dyn Fn(&str),
) -> Result<(Index, bool), String> {
    let volumes = open_volumes(letters, log)?;
    let path = &config.snapshot;

    if !config.rescan && path.exists() {
        let t = Instant::now();
        match Index::load(path) {
            Ok(index) => match snapshot_mismatch(&index, &volumes, config.skip_clutter) {
                None => {
                    let size = std::fs::metadata(path)
                        .map(|m| m.len() as usize)
                        .unwrap_or(0);
                    log(&format!(
                        "Loaded saved index ({} on disk) in {}",
                        fmt::bytes(size),
                        fmt::duration(t.elapsed())
                    ));
                    match catch_up_all(index, &volumes, log) {
                        Ok(index) => return Ok((index, false)),
                        Err(e) => log(&format!("Could not catch up on changes ({e}); rescanning.")),
                    }
                }
                Some(reason) => log(&format!(
                    "Saved index is out of date ({reason}); rescanning."
                )),
            },
            Err(e) => log(&format!("Could not load saved index ({e}); rescanning.")),
        }
    }

    let mut index = scan(&volumes, log)?;
    if config.skip_clutter {
        skip(&mut index, log);
    }
    let index =
        catch_up_all(index, &volumes, log).map_err(|e| format!("reading change journal: {e}"))?;
    let saved = save_to(&index, path, log);
    Ok((index, saved))
}

/// Why a snapshot cannot be used for these volumes, or `None` if it can.
fn snapshot_mismatch(index: &Index, volumes: &[OpenVolume], skip_clutter: bool) -> Option<String> {
    let wanted_rules = if skip_clutter { SKIP_RULES_VERSION } else { 0 };
    if index.skip_rules() != wanted_rules {
        return Some("the list of skipped folders changed".into());
    }
    let saved: Vec<&str> = index.volumes().iter().map(|v| v.label.as_str()).collect();
    let wanted: Vec<String> = volumes.iter().map(|v| format!("{}:", v.letter)).collect();
    if saved != wanted {
        return Some(format!(
            "it covers {} instead of {}",
            saved.join(" "),
            wanted.join(" ")
        ));
    }
    for (v, open) in index.volumes().iter().zip(volumes) {
        let letter = open.letter;
        let (Some(sync), Some(journal)) = (v.sync, open.journal) else {
            return Some(format!("drive {letter}: has no change journal"));
        };
        if sync.volume_serial != open.serial {
            return Some(format!("drive {letter}: is a different disk"));
        }
        if sync.journal_id != journal.journal_id {
            return Some(format!("drive {letter}: change journal was recreated"));
        }
        if sync.next_usn < journal.first_usn || sync.next_usn > journal.next_usn {
            return Some(format!("too many changes on {letter}: since last run"));
        }
    }
    None
}

fn scan(volumes: &[OpenVolume], log: &dyn Fn(&str)) -> Result<Index, String> {
    let mut builder = IndexBuilder::new();
    for open in volumes {
        let letter = open.letter;
        let t = Instant::now();
        builder.begin_volume(&format!("{letter}:"), bs_ntfs::ROOT_RECORD);
        let mut count = 0usize;
        let mut indexing = Duration::ZERO;
        let result = open.volume.enumerate(|record| {
            count += 1;
            let started = Instant::now();
            builder.push_utf16(
                record.record_number(),
                record.parent_record_number(),
                record.name,
                record.is_dir(),
                record.is_hidden_or_system(),
            );
            indexing += started.elapsed();
        });
        let read = t.elapsed().saturating_sub(indexing);
        let t_links = Instant::now();
        builder.end_volume();
        let linking = t_links.elapsed();
        result.map_err(|e| format!("reading drive {letter}: failed: {e}"))?;
        let journal = match open.journal {
            Some(_) => "live updates on",
            None => "no change journal, live updates off",
        };
        log(&format!(
            "{letter}:  {} entries in {}  (read {} · add {} · link {}; {journal})",
            fmt::count(count),
            fmt::duration(t.elapsed()),
            fmt::duration(read),
            fmt::duration(indexing),
            fmt::duration(linking)
        ));
    }
    let mut index = builder.finish();
    // Changes made during the scan are replayed from the journal position taken before it.
    for (i, open) in volumes.iter().enumerate() {
        index.set_sync(
            i,
            open.journal.map(|j| SyncPoint {
                volume_serial: open.serial,
                journal_id: j.journal_id,
                next_usn: j.next_usn,
            }),
        );
    }
    Ok(index)
}

/// Leaves out clutter folder contents and reports how much that removed.
pub fn skip(index: &mut Index, log: &dyn Fn(&str)) {
    let t = Instant::now();
    let before = index.live_len();
    let report = index.skip_clutter();
    log(&format!(
        "Skipped {} entries inside clutter folders ({:.0}% of {}) in {}",
        fmt::count(report.removed),
        100.0 * report.removed as f64 / before.max(1) as f64,
        fmt::count(before),
        fmt::duration(t.elapsed())
    ));
    for (rule, count) in report.by_rule.iter().take(15) {
        log(&format!("  {:>10}  {rule}", fmt::count(*count)));
    }
}

fn catch_up_all(mut index: Index, volumes: &[OpenVolume], log: &dyn Fn(&str)) -> io::Result<Index> {
    let t = Instant::now();
    let mut total = 0;
    for (i, open) in volumes.iter().enumerate() {
        let (Some(sync), Some(journal)) = (index.volumes()[i].sync, open.journal) else {
            continue;
        };
        let (next_usn, changes) = catch_up(&mut index, i, &open.volume, sync, journal.next_usn)?;
        index.set_sync(i, Some(SyncPoint { next_usn, ..sync }));
        total += changes;
    }
    if total > 0 {
        log(&format!(
            "Caught up on {} changes in {}",
            fmt::count(total),
            fmt::duration(t.elapsed())
        ));
    }
    Ok(index)
}

/// Applies journal records from `sync.next_usn` up to `until` without waiting.
fn catch_up(
    index: &mut Index,
    vol: usize,
    volume: &Volume,
    sync: SyncPoint,
    until: i64,
) -> io::Result<(i64, usize)> {
    let mut usn = sync.next_usn;
    let mut buffer = Vec::new();
    let mut name = String::new();
    let mut changes = 0;
    while usn < until {
        let next = volume.read_journal(sync.journal_id, usn, false, &mut buffer, |r| {
            if apply_record(index, vol, r, &mut name) != Applied::Unchanged {
                changes += 1;
            }
        })?;
        if next <= usn {
            break;
        }
        usn = next;
    }
    index.end_batch();
    Ok((usn, changes))
}

fn apply_record(index: &mut Index, vol: usize, r: &Record<'_>, name: &mut String) -> Applied {
    match r.change_kind() {
        ChangeKind::Skip => Applied::Unchanged,
        ChangeKind::Delete => index.apply(
            vol,
            Change::Delete {
                record: r.record_number(),
            },
        ),
        ChangeKind::Upsert => {
            name.clear();
            name.extend(
                char::decode_utf16(r.name.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')),
            );
            index.apply(
                vol,
                Change::Upsert {
                    record: r.record_number(),
                    parent_record: r.parent_record_number(),
                    name,
                    is_dir: r.is_dir(),
                    hidden: r.is_hidden_or_system(),
                },
            )
        }
    }
}

/// Journal record copied out so it can be applied after the blocking read returns.
struct OwnedRecord {
    kind: ChangeKind,
    record: u64,
    parent_record: u64,
    name: String,
    is_dir: bool,
    hidden: bool,
}

fn run_saver(shared: &Shared) {
    let mut last_save = Instant::now();
    while !shared.stopping() {
        thread::park_timeout(Duration::from_secs(15));
        if shared.stopping() {
            break;
        }
        if last_save.elapsed() >= SAVE_EVERY {
            save_shared(shared);
            last_save = Instant::now();
        }
        let idle = shared.last_search.lock().unwrap().elapsed() >= IDLE_TRIM_AFTER;
        if idle && !shared.trimmed.swap(true, Ordering::Relaxed) {
            memory::trim_working_set();
        }
    }
}

fn watch(shared: &Shared, vol: usize, label: &str, sync: SyncPoint) {
    let letter = label.chars().next().unwrap_or('?');
    let volume = match Volume::open(letter) {
        Ok(v) => v,
        Err(e) => {
            shared.event(format!("{label} live updates unavailable: {e}"));
            return;
        }
    };
    let mut usn = sync.next_usn;
    let mut buffer = Vec::new();
    let mut pending: Vec<OwnedRecord> = Vec::new();
    while !shared.stopping() {
        let started = Instant::now();
        pending.clear();
        let result = volume.read_journal(sync.journal_id, usn, true, &mut buffer, |r| {
            let kind = r.change_kind();
            if kind != ChangeKind::Skip {
                pending.push(OwnedRecord {
                    kind,
                    record: r.record_number(),
                    parent_record: r.parent_record_number(),
                    name: String::from_utf16_lossy(r.name),
                    is_dir: r.is_dir(),
                    hidden: r.is_hidden_or_system(),
                });
            }
        });
        let next = match result {
            Ok(next) => next,
            // A cancelled wait during shutdown; the saved position is still valid.
            Err(_) if shared.stopping() => return,
            Err(e) => {
                shared.event(format!(
                    "{label} live updates stopped ({e}); the next start rescans"
                ));
                let _maintenance = shared.maintenance.lock().unwrap();
                shared.index.write().unwrap().set_sync(vol, None);
                return;
            }
        };
        if pending.is_empty() && next == usn && started.elapsed() < Duration::from_millis(50) {
            // The wait returned at once with nothing new; back off instead of spinning.
            thread::sleep(Duration::from_millis(500));
            continue;
        }

        let _maintenance = shared.maintenance.lock().unwrap();
        let mut index = shared.index.write().unwrap();
        let mut lines = Vec::new();
        for r in &pending {
            let change = match r.kind {
                ChangeKind::Delete => Change::Delete { record: r.record },
                _ => Change::Upsert {
                    record: r.record,
                    parent_record: r.parent_record,
                    name: &r.name,
                    is_dir: r.is_dir,
                    hidden: r.hidden,
                },
            };
            let (mark, entry) = match index.apply(vol, change) {
                Applied::Created(e) => ('+', e),
                Applied::Updated(e) => ('~', e),
                Applied::Deleted(e) => ('-', e),
                Applied::Unchanged => continue,
            };
            lines.push((mark, entry));
        }
        index.end_batch();
        index.set_sync(
            vol,
            Some(SyncPoint {
                next_usn: next,
                ..sync
            }),
        );
        // Paths are built after the whole batch so renamed parents show their new name.
        let lines: Vec<String> = lines
            .iter()
            .rev()
            .take(LOG_LINES)
            .rev()
            .map(|&(mark, entry)| format!("{mark} {}", index.full_path(entry)))
            .collect();
        drop(index);
        for line in lines {
            shared.push_recent(line);
        }
        usn = next;
    }
}

fn save_to(index: &Index, path: &Path, log: &dyn Fn(&str)) -> bool {
    let t = Instant::now();
    match index.save(path) {
        Ok(()) => {
            let size = std::fs::metadata(path)
                .map(|m| m.len() as usize)
                .unwrap_or(0);
            log(&format!(
                "Saved index to {} ({}) in {}",
                path.display(),
                fmt::bytes(size),
                fmt::duration(t.elapsed())
            ));
            true
        }
        Err(e) => {
            log(&format!(
                "warning: could not save index to {}: {e}",
                path.display()
            ));
            false
        }
    }
}

/// Saves if anything changed since the last save, compacting first when enough garbage
/// has built up, then lets the index leave RAM again if nobody is searching. Searches
/// are never blocked: the compacted copy is built next to the live index and swapped in.
fn save_shared(shared: &Shared) {
    let Some(path) = &shared.snapshot else {
        return;
    };
    let _maintenance = shared.maintenance.lock().unwrap();
    let generation = shared.index.read().unwrap().generation();
    if *shared.saved_generation.lock().unwrap() == Some(generation) {
        return;
    }
    let needs_compaction = shared.index.read().unwrap().needs_compaction();
    if needs_compaction {
        let t = Instant::now();
        let fresh = shared.index.read().unwrap().compacted();
        let old = std::mem::replace(&mut *shared.index.write().unwrap(), fresh);
        // Freed outside the lock so searches do not wait for it.
        drop(old);
        shared.event(format!("compacted index in {}", fmt::duration(t.elapsed())));
    }
    // Saving only needs read access, so searches keep working meanwhile.
    let index = shared.index.read().unwrap();
    let t = Instant::now();
    match index.save(path) {
        Ok(()) => {
            *shared.saved_generation.lock().unwrap() = Some(index.generation());
            shared.event(format!("saved index in {}", fmt::duration(t.elapsed())));
        }
        Err(e) => shared.event(format!("could not save index: {e}")),
    }
    drop(index);
    // Compacting and saving read the whole index back into RAM.
    if shared.last_search.lock().unwrap().elapsed() >= IDLE_TRIM_AFTER {
        memory::trim_working_set();
        shared.trimmed.store(true, Ordering::Relaxed);
    }
}
