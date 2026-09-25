//! Saves the index to disk and loads it back, so startup does not need a full rescan.
//!
//! Layout: an 8-byte header (`BSIX` + format version), then one zstd stream holding the
//! name table, the entry arrays and the volumes. zstd's frame checksum catches
//! corruption; loading also validates every index so a bad file cannot cause panics.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::{Index, NO_PARENT, NameTable, Parts, SKIPPED_RECORD, SyncPoint, Volume};

const MAGIC: &[u8; 4] = b"BSIX";
const VERSION: u32 = 2;
const COMPRESSION_LEVEL: i32 = 3;
/// Refuse absurd lengths from a corrupt file instead of trying to allocate them.
const MAX_LEN: u64 = 1 << 32;

#[derive(Debug)]
pub enum SnapshotError {
    Io(io::Error),
    /// Written by a different version of the program.
    Version(u32),
    Invalid(&'static str),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Io(e) => write!(f, "{e}"),
            SnapshotError::Version(v) => {
                write!(f, "saved by format version {v}, expected {VERSION}")
            }
            SnapshotError::Invalid(what) => write!(f, "file is damaged ({what})"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<io::Error> for SnapshotError {
    fn from(e: io::Error) -> Self {
        SnapshotError::Io(e)
    }
}

impl Index {
    /// Writes the index to `path`, replacing any existing file only once the new one is
    /// complete, so a crash mid-save never leaves a broken snapshot behind.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut file = BufWriter::new(File::create(&tmp)?);
            file.write_all(MAGIC)?;
            file.write_all(&VERSION.to_le_bytes())?;
            let mut z = zstd::stream::write::Encoder::new(file, COMPRESSION_LEVEL)?;
            z.include_checksum(true)?;
            self.write_body(&mut z)?;
            z.finish()?
                .into_inner()
                .map_err(|e| e.into_error())?
                .sync_all()?;
        }
        fs::rename(&tmp, path)
    }

    fn write_body(&self, w: &mut impl Write) -> io::Result<()> {
        write_u32(w, self.skip_rules)?;
        write_bytes(w, &self.names.folded)?;
        write_u64s(w, &self.names.upper)?;
        write_u32s_delta(w, &self.names.offsets)?;
        write_u32s(w, &self.name_ids)?;
        write_u32s(w, &self.parents)?;
        write_bytes(w, &self.flags)?;
        write_u64(w, self.volumes.len() as u64)?;
        for v in &self.volumes {
            write_bytes(w, v.label.as_bytes())?;
            write_u32(w, v.root)?;
            write_u64(w, v.root_record)?;
            match v.sync {
                Some(s) => {
                    w.write_all(&[1])?;
                    write_u32(w, s.volume_serial)?;
                    write_u64(w, s.journal_id)?;
                    write_u64(w, s.next_usn as u64)?;
                }
                None => w.write_all(&[0])?,
            }
            write_u32s_delta(w, &v.record_lookup)?;
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Index, SnapshotError> {
        let mut file = BufReader::new(File::open(path)?);
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        if &header[..4] != MAGIC {
            return Err(SnapshotError::Invalid("not a snapshot"));
        }
        let version = u32::from_le_bytes(header[4..].try_into().unwrap());
        if version != VERSION {
            return Err(SnapshotError::Version(version));
        }
        let mut r = zstd::stream::read::Decoder::with_buffer(file)?;
        let index = read_body(&mut r)?;
        // Reading to the end makes zstd verify the frame checksum.
        let mut rest = Vec::new();
        r.read_to_end(&mut rest)?;
        if !rest.is_empty() {
            return Err(SnapshotError::Invalid("trailing data"));
        }
        Ok(index)
    }
}

fn read_body(r: &mut impl Read) -> Result<Index, SnapshotError> {
    let skip_rules = read_u32(r)?;
    let folded = read_bytes(r)?;
    let upper = read_u64s(r)?;
    let offsets = read_u32s_delta(r)?;
    let name_ids = read_u32s(r)?;
    let parents = read_u32s(r)?;
    let flags = read_bytes(r)?;
    let volume_count = read_u64(r)?;
    if volume_count > 64 {
        return Err(SnapshotError::Invalid("volume count"));
    }
    let mut volumes = Vec::new();
    for _ in 0..volume_count {
        let label = String::from_utf8(read_bytes(r)?)
            .map_err(|_| SnapshotError::Invalid("volume label"))?;
        let root = read_u32(r)?;
        let root_record = read_u64(r)?;
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag)?;
        let sync = match tag[0] {
            0 => None,
            1 => Some(SyncPoint {
                volume_serial: read_u32(r)?,
                journal_id: read_u64(r)?,
                next_usn: read_u64(r)? as i64,
            }),
            _ => return Err(SnapshotError::Invalid("volume sync tag")),
        };
        let record_lookup = read_u32s_delta(r)?;
        volumes.push(Volume {
            label,
            root,
            root_record,
            sync,
            record_lookup,
        });
    }

    let names = NameTable {
        folded,
        upper,
        offsets,
    };
    validate(&names, &name_ids, &parents, &flags, &volumes)?;
    Ok(Index::from_parts(Parts {
        names,
        name_ids,
        parents,
        flags,
        volumes,
        skip_rules,
    }))
}

fn validate(
    names: &NameTable,
    name_ids: &[u32],
    parents: &[u32],
    flags: &[u8],
    volumes: &[Volume],
) -> Result<(), SnapshotError> {
    let invalid = SnapshotError::Invalid;
    if names.offsets.first() != Some(&0)
        || *names.offsets.last().unwrap() as usize != names.folded.len()
    {
        return Err(invalid("name offsets"));
    }
    if names.offsets.windows(2).any(|w| w[0] > w[1]) {
        return Err(invalid("name offsets"));
    }
    if names.upper.len() != names.folded.len().div_ceil(64) {
        return Err(invalid("case bits"));
    }
    let text = std::str::from_utf8(&names.folded).map_err(|_| invalid("name text"))?;
    if names
        .offsets
        .iter()
        .any(|&o| !text.is_char_boundary(o as usize))
    {
        return Err(invalid("name boundaries"));
    }
    let entries = name_ids.len();
    if parents.len() != entries || flags.len() != entries {
        return Err(invalid("entry arrays"));
    }
    if name_ids.iter().any(|&id| id as usize >= names.len()) {
        return Err(invalid("entry names"));
    }
    if parents
        .iter()
        .any(|&p| p != NO_PARENT && p as usize >= entries)
    {
        return Err(invalid("entry parents"));
    }
    for v in volumes {
        if v.root as usize >= entries || parents[v.root as usize] != NO_PARENT {
            return Err(invalid("volume root"));
        }
        if v.record_lookup
            .iter()
            .any(|&e| e != NO_PARENT && e != SKIPPED_RECORD && e as usize >= entries)
        {
            return Err(invalid("volume records"));
        }
    }
    Ok(())
}

fn write_u32(w: &mut impl Write, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn write_u64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn write_bytes(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    write_u64(w, bytes.len() as u64)?;
    w.write_all(bytes)
}

/// Converts in chunks so large arrays are written with few calls and little extra memory.
const CHUNK: usize = 16 * 1024;

fn write_u32s(w: &mut impl Write, values: &[u32]) -> io::Result<()> {
    write_u64(w, values.len() as u64)?;
    let mut buf = Vec::with_capacity(CHUNK * 4);
    for chunk in values.chunks(CHUNK) {
        buf.clear();
        buf.extend(chunk.iter().flat_map(|v| v.to_le_bytes()));
        w.write_all(&buf)?;
    }
    Ok(())
}

/// Writes each value as the difference to the one before. Used for arrays that mostly
/// count upwards, which turns them into long runs of small numbers that compress about
/// ten times better.
fn write_u32s_delta(w: &mut impl Write, values: &[u32]) -> io::Result<()> {
    write_u64(w, values.len() as u64)?;
    let mut buf = Vec::with_capacity(CHUNK * 4);
    let mut prev = 0u32;
    for chunk in values.chunks(CHUNK) {
        buf.clear();
        for &v in chunk {
            buf.extend(v.wrapping_sub(prev).to_le_bytes());
            prev = v;
        }
        w.write_all(&buf)?;
    }
    Ok(())
}

fn read_u32s_delta(r: &mut impl Read) -> Result<Vec<u32>, SnapshotError> {
    let mut values = read_u32s(r)?;
    let mut prev = 0u32;
    for v in &mut values {
        prev = prev.wrapping_add(*v);
        *v = prev;
    }
    Ok(values)
}

fn write_u64s(w: &mut impl Write, values: &[u64]) -> io::Result<()> {
    write_u64(w, values.len() as u64)?;
    let mut buf = Vec::with_capacity(CHUNK * 8);
    for chunk in values.chunks(CHUNK) {
        buf.clear();
        buf.extend(chunk.iter().flat_map(|v| v.to_le_bytes()));
        w.write_all(&buf)?;
    }
    Ok(())
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_len(r: &mut impl Read) -> Result<usize, SnapshotError> {
    let len = read_u64(r)?;
    if len > MAX_LEN {
        return Err(SnapshotError::Invalid("length"));
    }
    Ok(len as usize)
}

fn read_bytes(r: &mut impl Read) -> Result<Vec<u8>, SnapshotError> {
    let len = read_len(r)?;
    let mut out = Vec::new();
    r.take(len as u64).read_to_end(&mut out)?;
    if out.len() != len {
        return Err(SnapshotError::Invalid("truncated"));
    }
    out.shrink_to_fit();
    Ok(out)
}

fn read_u32s(r: &mut impl Read) -> Result<Vec<u32>, SnapshotError> {
    let len = read_len(r)?;
    let mut out = Vec::with_capacity(len.min(CHUNK * 64));
    let mut buf = vec![0u8; CHUNK * 4];
    let mut left = len;
    while left > 0 {
        let n = left.min(CHUNK);
        r.read_exact(&mut buf[..n * 4])?;
        out.extend(
            buf[..n * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&b| u32::from_le_bytes(b)),
        );
        left -= n;
    }
    // The capacity was capped up front against corrupt lengths, so growth overshot.
    out.shrink_to_fit();
    Ok(out)
}

fn read_u64s(r: &mut impl Read) -> Result<Vec<u64>, SnapshotError> {
    let len = read_len(r)?;
    let mut out = Vec::with_capacity(len.min(CHUNK * 64));
    let mut buf = vec![0u8; CHUNK * 8];
    let mut left = len;
    while left > 0 {
        let n = left.min(CHUNK);
        r.read_exact(&mut buf[..n * 8])?;
        out.extend(
            buf[..n * 8]
                .as_chunks::<8>()
                .0
                .iter()
                .map(|&b| u64::from_le_bytes(b)),
        );
        left -= n;
    }
    out.shrink_to_fit();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{find, sample};
    use crate::{Change, Location};

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "bs-snapshot-test-{}-{name}.bin",
            std::process::id()
        ))
    }

    #[test]
    fn round_trips_everything() {
        let mut index = sample();
        index.set_sync(
            0,
            Some(SyncPoint {
                volume_serial: 0xABCD_1234,
                journal_id: 42,
                next_usn: 123_456,
            }),
        );
        index.apply(0, Change::Delete { record: 21 });
        index.end_batch();
        let path = temp_path("round-trip");
        index.save(&path).unwrap();
        let loaded = Index::load(&path).unwrap();
        fs::remove_file(&path).unwrap();

        assert_eq!(loaded.len(), index.len());
        assert_eq!(loaded.deleted_len(), 1);
        for e in 0..index.len() as u32 {
            assert_eq!(loaded.full_path(e), index.full_path(e));
            assert_eq!(loaded.flags()[e as usize], index.flags()[e as usize]);
        }
        assert_eq!(loaded.volumes()[0].sync, index.volumes()[0].sync);
        let report = find(&loaded, "C:\\Users\\bob\\Documents\\report.docx");
        assert_eq!(loaded.location(report), Location::UserContent);
        assert_eq!(loaded.volumes()[0].entry_for_record(13), Some(report));
    }

    #[test]
    fn loaded_index_accepts_changes() {
        let path = temp_path("changes");
        sample().save(&path).unwrap();
        let mut loaded = Index::load(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let names = loaded.names().len();
        loaded.apply(
            0,
            Change::Upsert {
                record: 60,
                parent_record: 31,
                name: "index.js",
                is_dir: false,
                hidden: false,
            },
        );
        loaded.end_batch();
        find(&loaded, "C:\\code\\index.js");
        assert_eq!(loaded.unmerged_names(), 1);
        loaded.compact();
        assert_eq!(
            loaded.names().len(),
            names,
            "duplicate name should be merged"
        );
        find(&loaded, "C:\\code\\index.js");
    }

    #[test]
    fn rejects_damaged_files() {
        let path = temp_path("damaged");
        sample().save(&path).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        fs::write(&path, &bytes).unwrap();
        assert!(Index::load(&path).is_err());

        bytes.truncate(12);
        fs::write(&path, &bytes).unwrap();
        assert!(Index::load(&path).is_err());

        fs::write(&path, b"hello world").unwrap();
        assert!(matches!(Index::load(&path), Err(SnapshotError::Invalid(_))));
        fs::remove_file(&path).unwrap();
    }
}
