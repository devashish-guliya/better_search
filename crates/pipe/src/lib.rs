//! How search windows talk to the better_search service: one message per request and
//! one per reply over the named pipe [`PIPE_NAME`].
//!
//! Request: `version u8 · kind u8 (1 = search) · limit u16 · query UTF-8 (rest)`.
//!
//! Reply: `version u8 · status u8 · total matches u32 · search time µs u32 ·
//! hit count u16`, then per hit `score i32 · flags u8 (1 = folder) · path length u16 ·
//! path UTF-8`. All numbers are little-endian.

mod client;

pub use client::Client;

pub const PIPE_NAME: &str = r"\\.\pipe\better_search";
pub const VERSION: u8 = 1;
/// Replies never carry more hits than this.
pub const MAX_LIMIT: u16 = 1000;
/// Longest request the service accepts.
pub const MAX_REQUEST: usize = 4096;

const KIND_SEARCH: u8 = 1;
const FLAG_DIR: u8 = 1;
const REQUEST_HEADER: usize = 4;
const REPLY_HEADER: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub query: String,
    pub limit: u16,
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
    /// Time the service spent searching and building paths, in microseconds.
    pub search_micros: u32,
    pub hits: Vec<Hit>,
}

impl Reply {
    pub fn status(status: Status) -> Self {
        Self {
            status,
            total_matches: 0,
            search_micros: 0,
            hits: Vec::new(),
        }
    }
}

impl Request {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        out.extend_from_slice(&[VERSION, KIND_SEARCH]);
        out.extend_from_slice(&self.limit.to_le_bytes());
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
        let query = std::str::from_utf8(&data[REQUEST_HEADER..])
            .ok()?
            .to_owned();
        Some(Self { query, limit })
    }
}

impl Reply {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        let count = self.hits.len().min(usize::from(MAX_LIMIT));
        out.extend_from_slice(&[VERSION, self.status.code()]);
        out.extend_from_slice(&self.total_matches.to_le_bytes());
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
        let search_micros = u32::from_le_bytes(data[6..10].try_into().ok()?);
        let count = u16::from_le_bytes([data[10], data[11]]);
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
        };
        let mut buf = Vec::new();
        request.encode(&mut buf);
        assert_eq!(Request::decode(&buf), Some(request));
    }

    #[test]
    fn request_limit_is_capped() {
        let mut buf = Vec::new();
        Request {
            query: "x".into(),
            limit: u16::MAX,
        }
        .encode(&mut buf);
        assert_eq!(Request::decode(&buf).unwrap().limit, MAX_LIMIT);
    }

    #[test]
    fn rejects_bad_requests() {
        assert_eq!(Request::decode(&[]), None);
        assert_eq!(
            Request::decode(&[VERSION + 1, KIND_SEARCH, 1, 0, b'a']),
            None
        );
        assert_eq!(Request::decode(&[VERSION, 9, 1, 0, b'a']), None);
        assert_eq!(Request::decode(&[VERSION, KIND_SEARCH, 1, 0, 0xff]), None);
        let mut long = vec![VERSION, KIND_SEARCH, 1, 0];
        long.resize(MAX_REQUEST + 1, b'a');
        assert_eq!(Request::decode(&long), None);
    }

    #[test]
    fn reply_round_trip() {
        let reply = Reply {
            status: Status::Ok,
            total_matches: 123_456,
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
}
