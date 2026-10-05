//! Answers searches on the named pipe: one thread waits for connections, and each
//! connection gets its own thread that serves requests until the client goes away.

use std::io;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use bs_engine::profiles::Profiles;
use bs_engine::{Engine, Log, fmt};
use bs_index::Index;
use bs_pipe::{Hit, MAX_REQUEST, PIPE_NAME, Reply, Request, StatsReply, Status};
use bs_query::{Query, Session};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_MORE_DATA, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::caller;
use crate::removable::{Removable, VolumeData};
use crate::security::{PIPE_SDDL, SecurityDescriptor};

/// More simultaneous connections than any number of search windows needs.
const MAX_CONNECTIONS: usize = 64;
const PIPE_BUFFER: u32 = 64 * 1024;
const PROFILE_CHECK_EVERY: Duration = Duration::from_secs(10);

pub struct State {
    pub engine: OnceLock<Engine>,
    profiles: Mutex<Option<Arc<Profiles>>>,
    log: Log,
    connections: AtomicUsize,
    /// Users already seen, so the log gets one line per user, not one per connection.
    seen_sids: Mutex<Vec<String>>,
    /// The profile folders last handed to the index, and when the registry was read.
    profile_folders: Mutex<(Option<Vec<String>>, Option<Instant>)>,
    removable: OnceLock<Arc<Removable>>,
}

impl State {
    pub fn new(log: Log) -> Self {
        Self {
            engine: OnceLock::new(),
            profiles: Mutex::new(None),
            log,
            connections: AtomicUsize::new(0),
            seen_sids: Mutex::new(Vec::new()),
            profile_folders: Mutex::new((None, None)),
            removable: OnceLock::new(),
        }
    }

    pub fn set_removable(&self, removable: &Arc<Removable>) {
        let _ = self.removable.set(Arc::clone(removable));
    }

    /// Gives the index the profile folders Windows lists, when they changed, so that
    /// profiles outside `<drive>\Users` are private too. Runs before a search, at most
    /// every [`PROFILE_CHECK_EVERY`], so a profile created while a search window stays
    /// connected is picked up within that time.
    fn refresh_profile_folders(&self, engine: &Engine) {
        let mut state = self.profile_folders.lock().unwrap();
        let (applied, checked) = &mut *state;
        if checked.is_some_and(|t| t.elapsed() < PROFILE_CHECK_EVERY) {
            return;
        }
        *checked = Some(Instant::now());
        let current = caller::profile_folders();
        if applied.as_ref() != Some(&current) {
            engine.set_profile_folders(&current);
            if let Some(removable) = self.removable.get() {
                removable.set_profile_folders(&current);
            }
            (self.log)(&format!(
                "profile folders in the registry: {}",
                current.len()
            ));
            *applied = Some(current);
        }
    }

    /// Reports the caller the first time a user connects.
    pub fn note_client(&self, sid: &str, profile: Option<&Path>) {
        let mut seen = self.seen_sids.lock().unwrap();
        if seen.iter().any(|s| s == sid) || seen.len() >= 64 {
            return;
        }
        seen.push(sid.to_owned());
        (self.log)(&format!(
            "client {sid} with profile {}",
            profile.map_or("(none)".into(), |p| p.display().to_string())
        ));
    }

    /// The owner map for the current index. Rebuilt outright when something could have
    /// moved profile ownership (the engine's epoch changed, which also covers
    /// compaction renumbering entries); otherwise only brought up to date with the
    /// entries added since, which is a 0.6 MB copy instead of a 5-9 ms full walk.
    fn profiles(&self, index: &Index, epoch: u64) -> Arc<Profiles> {
        let mut cached = self.profiles.lock().unwrap();
        if let Some(p) = &*cached
            && p.epoch() == epoch
        {
            if p.generation() == index.generation() {
                return Arc::clone(p);
            }
            let fresh = Arc::new(p.extended(index));
            *cached = Some(Arc::clone(&fresh));
            return fresh;
        }
        let first = cached.is_none();
        let t = Instant::now();
        let fresh = Arc::new(Profiles::build(index, epoch));
        if first {
            (self.log)(&format!(
                "built the profile map ({}) in {}",
                fmt::bytes(fresh.heap_bytes()),
                fmt::duration(t.elapsed())
            ));
        }
        *cached = Some(Arc::clone(&fresh));
        fresh
    }
}

struct Pipe(HANDLE);

// SAFETY: a pipe handle can be used from any thread.
unsafe impl Send for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateNamedPipeW and is closed only here.
        unsafe {
            DisconnectNamedPipe(self.0);
            CloseHandle(self.0);
        }
    }
}

fn create_instance(sd: &SecurityDescriptor, first: bool) -> io::Result<Pipe> {
    let name: Vec<u16> = PIPE_NAME.encode_utf16().chain([0]).collect();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.as_ptr(),
        bInheritHandle: 0,
    };
    // Refusing to open when the pipe already exists stops a second copy of the service,
    // and any other program that grabbed the name first, from sharing it.
    let first_flag = if first {
        FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        0
    };
    // SAFETY: `name` is NUL-terminated and `attributes` points to a valid descriptor.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | first_flag,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Pipe(handle))
}

/// Creates the pipe and starts accepting connections. Fails if the pipe already exists.
pub fn start(state: Arc<State>) -> io::Result<()> {
    let sd = SecurityDescriptor::from_sddl(PIPE_SDDL)?;
    let first = create_instance(&sd, true)?;
    thread::Builder::new()
        .name("pipe".into())
        .spawn(move || accept_loop(&state, &sd, first))?;
    Ok(())
}

fn accept_loop(state: &Arc<State>, sd: &SecurityDescriptor, mut pipe: Pipe) {
    loop {
        // SAFETY: the handle is a fresh, unconnected pipe instance.
        let ok = unsafe { ConnectNamedPipe(pipe.0, null_mut()) };
        let connected = ok != 0
            || io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32);
        let next = loop {
            match create_instance(sd, false) {
                Ok(next) => break next,
                Err(e) => {
                    (state.log)(&format!("cannot create a pipe instance: {e}"));
                    thread::sleep(Duration::from_secs(1));
                }
            }
        };
        let connection = std::mem::replace(&mut pipe, next);
        if !connected {
            continue;
        }
        if state.connections.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
            state.connections.fetch_sub(1, Ordering::Relaxed);
            continue;
        }
        let shared = Arc::clone(state);
        let spawned = thread::Builder::new().name("client".into()).spawn(move || {
            serve(&shared, &connection);
            shared.connections.fetch_sub(1, Ordering::Relaxed);
        });
        if let Err(e) = spawned {
            state.connections.fetch_sub(1, Ordering::Relaxed);
            (state.log)(&format!("cannot start a client thread: {e}"));
        }
    }
}

/// Viewer of this connection, looked up after its first request.
struct Viewer {
    profile: Option<PathBuf>,
}

fn serve(state: &State, pipe: &Pipe) {
    let mut session = Session::new();
    let mut viewer: Option<Viewer> = None;
    let mut request = vec![0u8; MAX_REQUEST];
    let mut out = Vec::new();
    loop {
        let Some(len) = read_message(pipe, &mut request) else {
            return;
        };
        let reply = match len {
            Err(()) => Reply::status(Status::BadRequest),
            Ok(len) => {
                let data = &request[..len];
                // A sizes request needs no viewer: it says nothing about any file, and a
                // window that is not allowed to search still may ask what things cost.
                if StatsReply::is_request(data) {
                    answer_stats(state).encode(&mut out);
                    if write_message(pipe, &out).is_err() {
                        return;
                    }
                    continue;
                }
                match Request::decode(data) {
                    None => Reply::status(Status::BadRequest),
                    Some(req) => {
                        if viewer.is_none() {
                            match caller::identify(pipe.0) {
                                Ok(c) => {
                                    state.note_client(&c.sid, c.profile.as_deref());
                                    viewer = Some(Viewer { profile: c.profile });
                                }
                                Err(e) => {
                                    (state.log)(&format!("could not identify a client: {e}"));
                                    Reply::status(Status::Denied).encode(&mut out);
                                    let _ = write_message(pipe, &out);
                                    return;
                                }
                            }
                        }
                        let viewer = viewer.as_ref().expect("identified above");
                        answer(state, &mut session, viewer, &req)
                    }
                }
            }
        };
        reply.encode(&mut out);
        if write_message(pipe, &out).is_err() {
            return;
        }
    }
}

/// What better_search costs, and what Windows search costs beside it.
fn answer_stats(state: &State) -> StatsReply {
    let memory = bs_engine::memory::process_memory();
    let engine = state.engine.get();
    let index_heap = engine.map(|engine| {
        let index = engine.read();
        let mut bytes = index.memory_usage().total() as u64;
        drop(index);
        if let Some(removable) = state.removable.get() {
            for data in removable.indexes() {
                bytes =
                    bytes.saturating_add(data.index.read().unwrap().memory_usage().total() as u64);
            }
        }
        bytes
    });
    let entries = engine.map(|engine| engine.read().live_len() as u64);
    let dir = bs_engine::machine_data_dir();
    let size_of = |path: PathBuf| std::fs::metadata(&path).ok().map(|m| m.len());
    let windows = crate::winsearch::footprint();
    StatsReply {
        // Without an engine the numbers below are not just missing, they are meaningless.
        status: if engine.is_some() {
            Status::Ok
        } else {
            Status::Loading
        },
        service_private: memory.as_ref().map(|m| m.private as u64),
        service_working_set: memory.as_ref().map(|m| m.working_set as u64),
        index_heap,
        snapshot_disk: size_of(dir.join("index.bin")),
        log_disk: size_of(dir.join("service.log")),
        service_binary_disk: std::env::current_exe().ok().and_then(size_of),
        entries,
        windows_search_memory: windows.memory,
        windows_search_disk: windows.disk,
    }
}

/// Reads one request. `None`: the client is gone. `Some(Err)`: the message was too long
/// (and has been drained).
fn read_message(pipe: &Pipe, buffer: &mut [u8]) -> Option<Result<usize, ()>> {
    let mut read = 0u32;
    // SAFETY: the buffer is valid for its length; no overlapped I/O.
    let ok = unsafe {
        ReadFile(
            pipe.0,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut read,
            null_mut(),
        )
    };
    if ok != 0 {
        return Some(Ok(read as usize));
    }
    if io::Error::last_os_error().raw_os_error() != Some(ERROR_MORE_DATA as i32) {
        return None;
    }
    loop {
        // SAFETY: as above.
        let ok = unsafe {
            ReadFile(
                pipe.0,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut read,
                null_mut(),
            )
        };
        if ok != 0 {
            return Some(Err(()));
        }
        if io::Error::last_os_error().raw_os_error() != Some(ERROR_MORE_DATA as i32) {
            return None;
        }
    }
}

fn write_message(pipe: &Pipe, data: &[u8]) -> io::Result<()> {
    let mut written = 0u32;
    // SAFETY: the buffer is valid for its length; no overlapped I/O.
    let ok = unsafe {
        WriteFile(
            pipe.0,
            data.as_ptr(),
            data.len() as u32,
            &mut written,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn answer(state: &State, session: &mut Session, viewer: &Viewer, req: &Request) -> Reply {
    let Some(engine) = state.engine.get() else {
        return Reply::status(Status::Loading);
    };
    let Some(query) = Query::parse(&req.query) else {
        return Reply::status(Status::Ok);
    };
    let query = query.hiding_system(!req.include_system);
    engine.touch();
    let t = Instant::now();
    state.refresh_profile_folders(engine);
    let index = engine.read();
    // The epoch is read from the same locked index, so it always describes this state.
    let profiles = state.profiles(&index, index.users_epoch());
    let visibility = profiles.visibility(viewer.profile.as_deref());
    let filter = |entry: u32| profiles.allows(&visibility, &index, entry);
    let result = session.search_filtered(&index, &query, usize::from(req.limit), Some(&filter));
    let mut hits: Vec<Hit> = result
        .hits
        .iter()
        .map(|hit| Hit {
            path: index.full_path(hit.entry),
            is_dir: index.is_dir(hit.entry),
            score: hit.score,
        })
        .collect();
    drop(index);
    let mut total = result.total_matches;
    let mut hidden = result.hidden_matches;
    if let Some(removable) = state.removable.get() {
        let volumes = removable.indexes();
        if !volumes.is_empty() {
            merge_removable(
                volumes,
                &query,
                viewer,
                usize::from(req.limit),
                (&mut total, &mut hidden),
                &mut hits,
            );
            // Stable ties keep the NTFS order before entries from removable drives.
            hits.sort_by_key(|a| std::cmp::Reverse(a.score));
            hits.truncate(usize::from(req.limit));
        }
    }
    Reply {
        status: Status::Ok,
        total_matches: u32::try_from(total).unwrap_or(u32::MAX),
        hidden_matches: u32::try_from(hidden).unwrap_or(u32::MAX),
        search_micros: u32::try_from(t.elapsed().as_micros()).unwrap_or(u32::MAX),
        hits,
    }
}

fn merge_removable(
    volumes: impl IntoIterator<Item = Arc<VolumeData>>,
    query: &Query,
    viewer: &Viewer,
    limit: usize,
    (total, hidden): (&mut usize, &mut usize),
    hits: &mut Vec<Hit>,
) {
    for data in volumes {
        let index = data.index.read().unwrap();
        let profiles = Arc::clone(&data.profiles.lock().unwrap());
        let visibility = profiles.visibility(viewer.profile.as_deref());
        let filter = |entry: u32| profiles.allows(&visibility, &index, entry);
        let found = bs_query::search_filtered(&index, query, limit, Some(&filter));
        *total = total.saturating_add(found.total_matches);
        *hidden = hidden.saturating_add(found.hidden_matches);
        hits.extend(found.hits.iter().map(|hit| Hit {
            path: index.full_path(hit.entry),
            is_dir: index.is_dir(hit.entry),
            score: hit.score,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bs_index::IndexBuilder;

    #[test]
    fn removable_counts_respect_profile_visibility() {
        let mut builder = IndexBuilder::new();
        builder.begin_volume("R:", 1);
        builder.push(2, 1, "Users", true, false);
        builder.push(3, 2, "alice", true, false);
        builder.push(4, 2, "bob", true, false);
        builder.push(5, 3, "needle.txt", false, false);
        builder.push(6, 4, "needle.txt", false, false);
        builder.push(7, 1, "needle.txt", false, false);
        builder.push(8, 1, "Private", true, false);
        builder.push(9, 8, "bob", true, false);
        builder.push(10, 9, "needle.txt", false, false);
        builder.end_volume();
        let data = Arc::new(VolumeData::new(
            builder.finish(),
            &[r"R:\Private\bob".into()],
        ));
        let query = Query::parse("needle").unwrap();
        let mut total = 0;
        let mut hidden = 0;
        let mut hits = Vec::new();
        let alice = Viewer {
            profile: Some(r"C:\Users\alice".into()),
        };
        merge_removable(
            [Arc::clone(&data)],
            &query,
            &alice,
            10,
            (&mut total, &mut hidden),
            &mut hits,
        );
        assert_eq!(total, 2);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| !h.path.contains(r"\bob\")));
        // A later registry refresh must also update the privacy map, not just NTFS.
        data.set_profile_folders(&[]);
        total = 0;
        hits.clear();
        merge_removable(
            [data],
            &query,
            &alice,
            10,
            (&mut total, &mut hidden),
            &mut hits,
        );
        assert_eq!(total, 3);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn sizes_say_loading_before_the_first_scan() {
        let state = State::new(Arc::new(|_: &str| {}));
        let reply = answer_stats(&state);
        assert_eq!(reply.status, Status::Loading);
        // A size that was never measured must be unknown, never a confident zero.
        assert_eq!(reply.index_heap, None);
        assert_eq!(reply.entries, None);
        // The service's own process exists whether or not the index does.
        assert!(reply.service_private.is_some_and(|bytes| bytes > 0));
        assert!(reply.service_working_set.is_some());
        // The binary it is running from is on disk either way.
        assert!(reply.service_binary_disk.is_some_and(|bytes| bytes > 0));
    }
}
