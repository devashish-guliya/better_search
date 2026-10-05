//! How search windows talk to the better_search service: one message per request and
//! one per reply over the named pipe [`PIPE_NAME`].
//!
//! Request: `version u8 · kind u8 (1 = search) · limit u16 · options u8 (1 = include
//! system and program folders) · query UTF-8 (rest)`.
//!
//! Reply: `version u8 · status u8 · total matches u32 · hidden matches u32 · search
//! time µs u32 · hit count u16`, then per hit `score i32 · flags u8 (1 = folder) · path
//! length u16 · path UTF-8`. Hidden matches are those left out because the request did
//! not include system and program folders. All numbers are little-endian.
//!
//! A read-only diagnostics request uses the same version and a second kind:
//!
//! Request: `version u8 · kind u8 (2 = sizes)`. Reply: `version u8 · status u8`, then
//! nine `u64` sizes in the order of [`StatsReply`], `u64::MAX` for one that is not
//! available yet. Nothing in it can change the service, so a window can ask for it
//! without any risk of disturbing a search.

mod client;

pub use client::Client;

pub const PIPE_NAME: &str = r"\\.\pipe\better_search";
pub const VERSION: u8 = 2;
/// Replies never carry more hits than this.
pub const MAX_LIMIT: u16 = 1000;
/// Longest request the service accepts.
pub const MAX_REQUEST: usize = 4096;

const KIND_SEARCH: u8 = 1;
const KIND_STATS: u8 = 2;
const FLAG_DIR: u8 = 1;
const OPTION_INCLUDE_SYSTEM: u8 = 1;
const REQUEST_HEADER: usize = 5;
const REPLY_HEADER: usize = 16;
/// Two bytes of header and the nine sizes of [`StatsReply`].
const STATS_REPLY_BYTES: usize = 2 + 9 * 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub query: String,
    pub limit: u16,
    /// Also search system, app-data and program folders. Otherwise their matches are
    /// only counted, in [`Reply::hidden_matches`].
    pub include_system: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// The service is still loading or scanning the drives.
    Loading,
    BadRequest,
    /// The caller could not be identified.
    Denied,
}

impl Status {
    fn code(self) -> u8 {
        match self {
            Status::Ok => 0,
            Status::Loading => 1,
            Status::BadRequest => 2,
            Status::Denied => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Status::Ok,
            1 => Status::Loading,
            2 => Status::BadRequest,
            3 => Status::Denied,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub is_dir: bool,
    pub score: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: Status,
    pub total_matches: u32,
    /// Matches in system and program folders that were left out of the reply.
    pub hidden_matches: u32,
    /// Time the service spent searching and building paths, in microseconds.
    pub search_micros: u32,
    pub hits: Vec<Hit>,
}

impl Reply {
    pub fn status(status: Status) -> Self {
        Self {
            status,
            total_matches: 0,
            hidden_matches: 0,
            search_micros: 0,
            hits: Vec::new(),
        }
    }
}

/// What better_search costs and what it holds, in bytes. `None` means the number is not
/// known yet, which is not the same as zero: the first scan is still running, or a file
/// could not be read.
///
/// Both memory figures are working sets, the number Task Manager's Memory column shows, so
/// the two can be compared by looking at them side by side. The disk figures are the index
/// each one keeps, not the size of the programs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatsReply {
    pub status: Status,
    /// Committed memory of the service process, which stays reserved even when trimmed.
    pub service_private: Option<u64>,
    /// Memory of the service process in RAM right now.
    pub service_working_set: Option<u64>,
    /// The index in memory. Part of `service_private`, not additional to it.
    pub index_heap: Option<u64>,
    /// The saved index on disk.
    pub snapshot_disk: Option<u64>,
    pub log_disk: Option<u64>,
    pub service_binary_disk: Option<u64>,
    /// Files and folders the index holds.
    pub entries: Option<u64>,
    /// What Windows search is using right now, so a window can show both side by side.
    /// Measured by the service, which is the only part of better_search allowed to read
    /// Windows' own search folders.
    pub windows_search_memory: Option<u64>,
    pub windows_search_disk: Option<u64>,
}

impl StatsReply {
    /// The request bytes that ask for this reply.
    pub fn request() -> [u8; 2] {
        [VERSION, KIND_STATS]
    }

    pub fn is_request(data: &[u8]) -> bool {
        data == Self::request()
    }

    /// Everything unknown, for a caller that may not be told or could not be asked.
    pub fn unavailable(status: Status) -> Self {
        Self {
            status,
            service_private: None,
            service_working_set: None,
            index_heap: None,
            snapshot_disk: None,
            log_disk: None,
            service_binary_disk: None,
            entries: None,
            windows_search_memory: None,
            windows_search_disk: None,
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        out.extend_from_slice(&[VERSION, self.status.code()]);
        for size in [
            self.service_private,
            self.service_working_set,
            self.index_heap,
            self.snapshot_disk,
            self.log_disk,
            self.service_binary_disk,
            self.entries,
            self.windows_search_memory,
            self.windows_search_disk,
        ] {
            out.extend_from_slice(&size.unwrap_or(u64::MAX).to_le_bytes());
        }
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() != STATS_REPLY_BYTES || data[0] != VERSION {
            return None;
        }
        let field = |offset: usize| {
            let raw = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            (raw != u64::MAX).then_some(raw)
        };
        Some(Self {
            status: Status::from_code(data[1])?,
            service_private: field(2),
            service_working_set: field(10),
            index_heap: field(18),
            snapshot_disk: field(26),
            log_disk: field(34),
            service_binary_disk: field(42),
            entries: field(50),
            windows_search_memory: field(58),
            windows_search_disk: field(66),
        })
    }
}

impl Request {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        out.extend_from_slice(&[VERSION, KIND_SEARCH]);
        out.extend_from_slice(&self.limit.to_le_bytes());
        out.push(if self.include_system {
            OPTION_INCLUDE_SYSTEM
        } else {
            0
        });
        out.extend_from_slice(self.query.as_bytes());
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < REQUEST_HEADER || data.len() > MAX_REQUEST {
            return None;
        }
        if data[0] != VERSION || data[1] != KIND_SEARCH {
            return None;
        }
        let limit = u16::from_le_bytes([data[2], data[3]]).min(MAX_LIMIT);
        let include_system = data[4] & OPTION_INCLUDE_SYSTEM != 0;
        let query = std::str::from_utf8(&data[REQUEST_HEADER..])
            .ok()?
            .to_owned();
        Some(Self {
            query,
            limit,
            include_system,
        })
    }
}

impl Reply {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        let count = self.hits.len().min(usize::from(MAX_LIMIT));
        out.extend_from_slice(&[VERSION, self.status.code()]);
        out.extend_from_slice(&self.total_matches.to_le_bytes());
        out.extend_from_slice(&self.hidden_matches.to_le_bytes());
        out.extend_from_slice(&self.search_micros.to_le_bytes());
        out.extend_from_slice(&(count as u16).to_le_bytes());
        for hit in &self.hits[..count] {
            let path = truncate_utf8(&hit.path, usize::from(u16::MAX));
            out.extend_from_slice(&hit.score.to_le_bytes());
            out.push(if hit.is_dir { FLAG_DIR } else { 0 });
            out.extend_from_slice(&(path.len() as u16).to_le_bytes());
            out.extend_from_slice(path.as_bytes());
        }
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < REPLY_HEADER || data[0] != VERSION {
            return None;
        }
        let status = Status::from_code(data[1])?;
        let total_matches = u32::from_le_bytes(data[2..6].try_into().ok()?);
        let hidden_matches = u32::from_le_bytes(data[6..10].try_into().ok()?);
        let search_micros = u32::from_le_bytes(data[10..14].try_into().ok()?);
        let count = u16::from_le_bytes([data[14], data[15]]);
        let mut hits = Vec::with_capacity(usize::from(count.min(MAX_LIMIT)));
        let mut rest = &data[REPLY_HEADER..];
        for _ in 0..count {
            let (head, tail) = rest.split_at_checked(7)?;
            let score = i32::from_le_bytes(head[0..4].try_into().ok()?);
            let is_dir = head[4] & FLAG_DIR != 0;
            let len = usize::from(u16::from_le_bytes([head[5], head[6]]));
            let (path, tail) = tail.split_at_checked(len)?;
            hits.push(Hit {
                path: std::str::from_utf8(path).ok()?.to_owned(),
                is_dir,
                score,
            });
            rest = tail;
        }
        rest.is_empty().then_some(Self {
            status,
            total_matches,
            hidden_matches,
            search_micros,
            hits,
        })
    }
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip() {
        let request = Request {
            query: "tax 2024 änd".into(),
            limit: 50,
            include_system: false,
        };
        let mut buf = Vec::new();
        request.encode(&mut buf);
        assert_eq!(Request::decode(&buf), Some(request));
        let request = Request {
            query: "x".into(),
            limit: 1,
            include_system: true,
        };
        request.encode(&mut buf);
        assert_eq!(Request::decode(&buf), Some(request));
    }

    #[test]
    fn request_limit_is_capped() {
        let mut buf = Vec::new();
        Request {
            query: "x".into(),
            limit: u16::MAX,
            include_system: false,
        }
        .encode(&mut buf);
        assert_eq!(Request::decode(&buf).unwrap().limit, MAX_LIMIT);
    }

    #[test]
    fn rejects_bad_requests() {
        assert_eq!(Request::decode(&[]), None);
        assert_eq!(
            Request::decode(&[VERSION + 1, KIND_SEARCH, 1, 0, 0, b'a']),
            None
        );
        assert_eq!(Request::decode(&[VERSION, 9, 1, 0, 0, b'a']), None);
        assert_eq!(
            Request::decode(&[VERSION, KIND_SEARCH, 1, 0, 0, 0xff]),
            None
        );
        let mut long = vec![VERSION, KIND_SEARCH, 1, 0, 0];
        long.resize(MAX_REQUEST + 1, b'a');
        assert_eq!(Request::decode(&long), None);
    }

    #[test]
    fn reply_round_trip() {
        let reply = Reply {
            status: Status::Ok,
            total_matches: 123_456,
            hidden_matches: 4_000_000,
            search_micros: 789,
            hits: vec![
                Hit {
                    path: r"C:\Users\anna\Documents".into(),
                    is_dir: true,
                    score: 128,
                },
                Hit {
                    path: r"D:\Fotos\Ölberg.jpg".into(),
                    is_dir: false,
                    score: -12,
                },
            ],
        };
        let mut buf = Vec::new();
        reply.encode(&mut buf);
        assert_eq!(Reply::decode(&buf), Some(reply));
        assert_eq!(
            Reply::decode(&{
                Reply::status(Status::Loading).encode(&mut buf);
                buf.clone()
            })
            .unwrap()
            .status,
            Status::Loading
        );
    }

    #[test]
    fn rejects_truncated_replies() {
        let reply = Reply {
            status: Status::Ok,
            total_matches: 1,
            hidden_matches: 0,
            search_micros: 1,
            hits: vec![Hit {
                path: "C:\\a".into(),
                is_dir: false,
                score: 1,
            }],
        };
        let mut buf = Vec::new();
        reply.encode(&mut buf);
        for len in 0..buf.len() {
            assert_eq!(Reply::decode(&buf[..len]), None, "length {len}");
        }
        buf.push(0);
        assert_eq!(Reply::decode(&buf), None);
    }

    #[test]
    fn a_sizes_request_is_not_a_search() {
        assert!(StatsReply::is_request(&StatsReply::request()));
        // A search for the empty query is the same length, and must not be mistaken for it.
        let mut search = Vec::new();
        Request {
            query: String::new(),
            limit: 0,
            include_system: false,
        }
        .encode(&mut search);
        assert!(!StatsReply::is_request(&search));
        assert_eq!(Request::decode(&StatsReply::request()), None);
    }

    #[test]
    fn sizes_round_trip_and_keep_unknown_apart_from_zero() {
        let reply = StatsReply {
            status: Status::Ok,
            service_private: Some(21 * 1024 * 1024),
            service_working_set: Some(0),
            index_heap: Some(18 * 1024 * 1024),
            snapshot_disk: Some(4_300_000),
            log_disk: Some(1_702),
            service_binary_disk: None,
            entries: Some(574_455),
            windows_search_memory: Some(19 * 1024 * 1024),
            windows_search_disk: Some(34 * 1024 * 1024),
        };
        let mut buf = Vec::new();
        reply.encode(&mut buf);
        assert_eq!(buf.len(), STATS_REPLY_BYTES);
        assert_eq!(StatsReply::decode(&buf), Some(reply));
        // A zero is a real measurement, and must not read back as "not available".
        assert_eq!(
            StatsReply::decode(&buf).unwrap().service_working_set,
            Some(0)
        );
    }

    #[test]
    fn rejects_malformed_sizes_replies() {
        let mut buf = Vec::new();
        StatsReply::unavailable(Status::Loading).encode(&mut buf);
        for len in 0..buf.len() {
            assert_eq!(StatsReply::decode(&buf[..len]), None, "length {len}");
        }
        let mut long = buf.clone();
        long.push(0);
        assert_eq!(StatsReply::decode(&long), None);
        let mut wrong_version = buf.clone();
        wrong_version[0] = VERSION + 1;
        assert_eq!(StatsReply::decode(&wrong_version), None);
        let mut bad_status = buf.clone();
        bad_status[1] = 9;
        assert_eq!(StatsReply::decode(&bad_status), None);
        // A sizes reply of the full length with every field unknown is valid.
        let mut all_unknown = [u64::MAX.to_le_bytes(); 9].concat();
        all_unknown.insert(0, Status::Ok.code());
        all_unknown.insert(0, VERSION);
        assert_eq!(all_unknown.len(), STATS_REPLY_BYTES);
        assert_eq!(
            StatsReply::decode(&all_unknown),
            Some(StatsReply::unavailable(Status::Ok))
        );
        assert_eq!(StatsReply::decode(&[VERSION, 0]), None);
    }
}
