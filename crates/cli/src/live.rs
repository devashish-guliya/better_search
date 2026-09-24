//! Keeps an NTFS index current: loads the saved snapshot (or scans), catches up on
//! changes made while the program was closed, then follows each volume's change
//! journal on its own thread.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use bs_index::{Applied, Change, Index, IndexBuilder, SyncPoint};
use bs_ntfs::{ChangeKind, JournalInfo, Record, Volume};

use crate::{fmt_bytes, fmt_count, fmt_duration};

const LOG_LINES: usize = 50;
const SAVE_EVERY: Duration = Duration::from_secs(5 * 60);
/// Same size Windows uses for the journal it creates on the system drive.
const JOURNAL_MAX_BYTES: u64 = 32 << 20;
const JOURNAL_GROW_BYTES: u64 = 8 << 20;

/// Index plus what the background threads report.
pub struct Shared {
    pub index: RwLock<Index>,
    log: Mutex<VecDeque<String>>,
    saved_generation: Mutex<Option<u64>>,
    stop: AtomicBool,
}

impl Shared {
    pub fn new(index: Index) -> Self {
        Self {
            index: RwLock::new(index),
            log: Mutex::new(VecDeque::new()),
            saved_generation: Mutex::new(None),
            stop: AtomicBool::new(false),
        }
    }

    fn push_log(&self, line: String) {
        let mut log = self.log.lock().unwrap();
        if log.len() == LOG_LINES {
            log.pop_front();
        }
        log.push_back(line);
    }

    pub fn recent_changes(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().cloned().collect()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn snapshot_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("better_search").join("index.bin")
}

struct OpenVolume {
    letter: char,
    volume: Volume,
    serial: u32,
    journal: Option<JournalInfo>,
}

fn open_volumes(letters: &[char]) -> Result<Vec<OpenVolume>, String> {
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
                        println!("Turned on the change journal of drive {letter}:");
                        volume.journal()
                    }
                    Err(e) => {
                        println!("Could not turn on the change journal of drive {letter}: ({e})");
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

/// Loads or builds the index for `letters` and brings it fully up to date. The flag
/// tells whether the returned index is already saved to disk.
pub fn load_or_scan(letters: &[char], rescan: bool) -> Result<(Index, bool), String> {
    let volumes = open_volumes(letters)?;
    let path = snapshot_path();

    if !rescan && path.exists() {
        let t = Instant::now();
        match Index::load(&path) {
            Ok(index) => match snapshot_mismatch(&index, &volumes) {
                None => {
                    let size = std::fs::metadata(&path)
                        .map(|m| m.len() as usize)
                        .unwrap_or(0);
                    println!(
                        "Loaded saved index ({} on disk) in {}",
                        fmt_bytes(size),
                        fmt_duration(t.elapsed())
                    );
                    match catch_up_all(index, &volumes) {
                        Ok(index) => return Ok((index, false)),
                        Err(e) => println!("Could not catch up on changes ({e}); rescanning."),
                    }
                }
                Some(reason) => println!("Saved index is out of date ({reason}); rescanning."),
            },
            Err(e) => println!("Could not load saved index ({e}); rescanning."),
        }
    }

    let index = scan(&volumes)?;
    let index =
        catch_up_all(index, &volumes).map_err(|e| format!("reading change journal: {e}"))?;
    let saved = save(&index, &path);
    Ok((index, saved))
}

/// Why a snapshot cannot be used for these volumes, or `None` if it can.
fn snapshot_mismatch(index: &Index, volumes: &[OpenVolume]) -> Option<String> {
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

fn scan(volumes: &[OpenVolume]) -> Result<Index, String> {
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
        println!(
            "{letter}:  {} entries in {}  (read {} · add {} · link {}; {journal})",
            fmt_count(count),
            fmt_duration(t.elapsed()),
            fmt_duration(read),
            fmt_duration(indexing),
            fmt_duration(linking)
        );
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

fn catch_up_all(mut index: Index, volumes: &[OpenVolume]) -> io::Result<Index> {
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
        println!(
            "Caught up on {} changes in {}",
            fmt_count(total),
            fmt_duration(t.elapsed())
        );
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

/// Starts one watcher thread per volume with a change journal, plus the periodic saver.
pub fn start_background(shared: &Arc<Shared>) {
    let volumes: Vec<(usize, String, SyncPoint)> = shared
        .index
        .read()
        .unwrap()
        .volumes()
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.sync.map(|s| (i, v.label.clone(), s)))
        .collect();
    for (vol, label, sync) in volumes {
        let shared = Arc::clone(shared);
        thread::Builder::new()
            .name(format!("watch {label}"))
            .spawn(move || watch(&shared, vol, &label, sync))
            .expect("spawning a thread");
    }
    let shared = Arc::clone(shared);
    thread::Builder::new()
        .name("saver".into())
        .spawn(move || {
            while !shared.stop.load(Ordering::Relaxed) {
                thread::park_timeout(SAVE_EVERY);
                if !shared.stop.load(Ordering::Relaxed) {
                    save_shared(&shared);
                }
            }
        })
        .expect("spawning a thread");
}

fn watch(shared: &Shared, vol: usize, label: &str, sync: SyncPoint) {
    let letter = label.chars().next().unwrap_or('?');
    let volume = match Volume::open(letter) {
        Ok(v) => v,
        Err(e) => {
            shared.push_log(format!("{label} live updates unavailable: {e}"));
            return;
        }
    };
    let mut usn = sync.next_usn;
    let mut buffer = Vec::new();
    let mut pending: Vec<OwnedRecord> = Vec::new();
    while !shared.stop.load(Ordering::Relaxed) {
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
            Err(e) => {
                shared.push_log(format!(
                    "{label} live updates stopped ({e}); the next start rescans"
                ));
                shared.index.write().unwrap().set_sync(vol, None);
                return;
            }
        };
        if pending.is_empty() && next == usn && started.elapsed() < Duration::from_millis(50) {
            // The wait returned at once with nothing new; back off instead of spinning.
            thread::sleep(Duration::from_millis(500));
            continue;
        }

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
            shared.push_log(line);
        }
        usn = next;
    }
}

fn save(index: &Index, path: &std::path::Path) -> bool {
    let t = Instant::now();
    match index.save(path) {
        Ok(()) => {
            let size = std::fs::metadata(path)
                .map(|m| m.len() as usize)
                .unwrap_or(0);
            println!(
                "Saved index to {} ({}) in {}",
                path.display(),
                fmt_bytes(size),
                fmt_duration(t.elapsed())
            );
            true
        }
        Err(e) => {
            eprintln!("warning: could not save index to {}: {e}", path.display());
            false
        }
    }
}

/// Compacts and saves if anything changed since the last save.
pub fn save_shared(shared: &Shared) {
    let generation = shared.index.read().unwrap().generation();
    if *shared.saved_generation.lock().unwrap() == Some(generation) {
        return;
    }
    {
        let mut index = shared.index.write().unwrap();
        if index.deleted_len() > 0 {
            index.compact();
        }
    }
    // Saving only needs read access, so searches keep working meanwhile.
    let index = shared.index.read().unwrap();
    let t = Instant::now();
    let path = snapshot_path();
    match index.save(&path) {
        Ok(()) => {
            *shared.saved_generation.lock().unwrap() = Some(index.generation());
            shared.push_log(format!("saved index in {}", fmt_duration(t.elapsed())));
        }
        Err(e) => shared.push_log(format!("could not save index: {e}")),
    }
}

/// Marks the current state as saved, e.g. right after a load or scan wrote it.
pub fn mark_saved(shared: &Shared) {
    let generation = shared.index.read().unwrap().generation();
    *shared.saved_generation.lock().unwrap() = Some(generation);
}
