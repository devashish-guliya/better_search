//! Reads NTFS volumes directly:
//! - the complete file and folder list with `FSCTL_ENUM_USN_DATA`, which is far faster
//!   than walking directories;
//! - the change journal with `FSCTL_READ_USN_JOURNAL`, to keep an index up to date.
//!
//! Opening a volume needs administrator rights.

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_HANDLE_EOF, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

/// MFT record number of the root directory on every NTFS volume.
pub const ROOT_RECORD: u64 = 5;

// CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 44, METHOD_NEITHER, FILE_ANY_ACCESS)
const FSCTL_ENUM_USN_DATA: u32 = 0x0009_00B3;
// CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 46, METHOD_NEITHER, FILE_ANY_ACCESS)
const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00BB;
// CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 61, METHOD_BUFFERED, FILE_ANY_ACCESS)
const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00F4;
const DRIVE_FIXED: u32 = 3;

const ENUM_BUFFER_BYTES: usize = 1 << 20;
const JOURNAL_BUFFER_BYTES: usize = 64 * 1024;

/// `USN_REASON_*` flags: why a change journal record was written.
pub mod reason {
    pub const FILE_CREATE: u32 = 0x0000_0100;
    pub const FILE_DELETE: u32 = 0x0000_0200;
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
    pub const BASIC_INFO_CHANGE: u32 = 0x0000_8000;
    pub const HARD_LINK_CHANGE: u32 = 0x0001_0000;
}

/// Only changes that affect names, locations or attributes; content writes are skipped.
const JOURNAL_REASON_MASK: u32 = reason::FILE_CREATE
    | reason::FILE_DELETE
    | reason::RENAME_OLD_NAME
    | reason::RENAME_NEW_NAME
    | reason::BASIC_INFO_CHANGE
    | reason::HARD_LINK_CHANGE;

#[repr(C)]
struct MftEnumDataV0 {
    start_file_reference_number: u64,
    low_usn: i64,
    high_usn: i64,
}

#[repr(C)]
struct ReadUsnJournalDataV0 {
    start_usn: i64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    usn_journal_id: u64,
}

#[repr(C)]
#[derive(Default)]
struct UsnJournalDataV0 {
    usn_journal_id: u64,
    first_usn: i64,
    next_usn: i64,
    lowest_valid_usn: i64,
    max_usn: i64,
    maximum_size: u64,
    allocation_delta: u64,
}

/// State of a volume's change journal.
#[derive(Clone, Copy, Debug)]
pub struct JournalInfo {
    /// Changes whenever the journal is deleted and recreated.
    pub journal_id: u64,
    /// Oldest position still available; older positions have been overwritten.
    pub first_usn: i64,
    /// Position the next change will be written at.
    pub next_usn: i64,
}

/// What a record means for an index of names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// The file or folder exists with this name, parent and attributes.
    Upsert,
    Delete,
    /// The old name of a rename; the record with the new name follows.
    Skip,
}

/// One file or folder as reported by NTFS.
pub struct Record<'a> {
    pub file_reference: u64,
    pub parent_reference: u64,
    pub attributes: u32,
    /// `USN_REASON_*` flags; always 0 in a full enumeration.
    pub reason: u32,
    pub usn: i64,
    pub name: &'a [u16],
}

impl Record<'_> {
    /// MFT record number: the file reference without its sequence number.
    pub fn record_number(&self) -> u64 {
        record_number(self.file_reference)
    }

    pub fn parent_record_number(&self) -> u64 {
        record_number(self.parent_reference)
    }

    pub fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }

    pub fn is_hidden_or_system(&self) -> bool {
        self.attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
    }

    pub fn change_kind(&self) -> ChangeKind {
        if self.reason & reason::FILE_DELETE != 0 {
            ChangeKind::Delete
        } else if self.reason & reason::RENAME_OLD_NAME != 0
            && self.reason & reason::RENAME_NEW_NAME == 0
        {
            ChangeKind::Skip
        } else {
            ChangeKind::Upsert
        }
    }
}

pub fn record_number(file_reference: u64) -> u64 {
    file_reference & 0x0000_FFFF_FFFF_FFFF
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct VolumeInfo {
    serial: u32,
    file_system: String,
}

fn volume_info(letter: char) -> io::Result<VolumeInfo> {
    let root = wide(&format!("{letter}:\\"));
    let mut serial = 0u32;
    let mut fs_name = [0u16; 32];
    // SAFETY: `root` is NUL-terminated; unused outputs are null; buffer sizes are correct.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            null_mut(),
            0,
            &mut serial,
            null_mut(),
            null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let len = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    Ok(VolumeInfo {
        serial,
        file_system: String::from_utf16_lossy(&fs_name[..len]),
    })
}

/// Serial number of the volume, used to notice when a drive letter now points at a
/// different disk.
pub fn volume_serial(letter: char) -> io::Result<u32> {
    volume_info(letter).map(|info| info.serial)
}

/// Drive letters of all fixed (non-removable) NTFS volumes.
pub fn fixed_ntfs_volumes() -> Vec<char> {
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| char::from(b'A' + i))
        .filter(|&letter| {
            let root = wide(&format!("{letter}:\\"));
            // SAFETY: `root` is a NUL-terminated UTF-16 string.
            let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
            drive_type == DRIVE_FIXED
                && volume_info(letter).is_ok_and(|info| info.file_system == "NTFS")
        })
        .collect()
}

/// An open handle to a volume such as `\\.\C:`.
///
/// Calls on one handle run one at a time, so a thread blocked waiting for journal
/// changes should use its own `Volume`.
pub struct Volume {
    handle: HANDLE,
}

// SAFETY: a volume handle can be used from any thread.
unsafe impl Send for Volume {}

impl Volume {
    /// Opens the volume for reading. Fails with `PermissionDenied` without admin rights.
    pub fn open(letter: char) -> io::Result<Self> {
        let path = wide(&format!(r"\\.\{letter}:"));
        // SAFETY: `path` is NUL-terminated; other pointer arguments are allowed to be null.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// Returns `None` if the change journal is not active on this volume.
    pub fn journal(&self) -> Option<JournalInfo> {
        let mut data = UsnJournalDataV0::default();
        let mut returned = 0u32;
        // SAFETY: output buffer and its size match; no input buffer.
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                FSCTL_QUERY_USN_JOURNAL,
                null(),
                0,
                (&raw mut data).cast::<c_void>(),
                size_of::<UsnJournalDataV0>() as u32,
                &mut returned,
                null_mut(),
            )
        };
        (ok != 0).then_some(JournalInfo {
            journal_id: data.usn_journal_id,
            first_usn: data.first_usn,
            next_usn: data.next_usn,
        })
    }

    /// Calls `on_record` for every file and folder on the volume, in MFT order.
    /// The root folder itself is not reported.
    pub fn enumerate(&self, mut on_record: impl FnMut(&Record<'_>)) -> io::Result<()> {
        let mut input = MftEnumDataV0 {
            start_file_reference_number: 0,
            low_usn: 0,
            high_usn: i64::MAX,
        };
        let mut buffer = vec![0u8; ENUM_BUFFER_BYTES];
        let mut name = Vec::with_capacity(256);
        loop {
            let mut returned = 0u32;
            // SAFETY: input and output buffers are valid for the sizes passed.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle,
                    FSCTL_ENUM_USN_DATA,
                    (&raw const input).cast::<c_void>(),
                    size_of::<MftEnumDataV0>() as u32,
                    buffer.as_mut_ptr().cast::<c_void>(),
                    buffer.len() as u32,
                    &mut returned,
                    null_mut(),
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
                    return Ok(());
                }
                return Err(err);
            }
            let returned = returned as usize;
            if returned <= 8 {
                return Ok(());
            }
            // The output starts with the file reference to continue from.
            input.start_file_reference_number = read_u64(&buffer, 0);
            parse_records(&buffer[8..returned], &mut name, &mut on_record);
        }
    }

    /// Reads change journal records starting at `start_usn` and returns the position to
    /// continue from. With `wait`, blocks until at least one new change exists.
    ///
    /// Any error means the index can no longer be trusted to be complete (the journal
    /// was deleted, recreated, or `start_usn` was already overwritten) and the volume
    /// needs a full rescan.
    pub fn read_journal(
        &self,
        journal_id: u64,
        start_usn: i64,
        wait: bool,
        buffer: &mut Vec<u8>,
        mut on_record: impl FnMut(&Record<'_>),
    ) -> io::Result<i64> {
        buffer.resize(JOURNAL_BUFFER_BYTES, 0);
        let input = ReadUsnJournalDataV0 {
            start_usn,
            reason_mask: JOURNAL_REASON_MASK,
            return_only_on_close: 0,
            timeout: 0,
            bytes_to_wait_for: u64::from(wait),
            usn_journal_id: journal_id,
        };
        let mut returned = 0u32;
        // SAFETY: input and output buffers are valid for the sizes passed.
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                FSCTL_READ_USN_JOURNAL,
                (&raw const input).cast::<c_void>(),
                size_of::<ReadUsnJournalDataV0>() as u32,
                buffer.as_mut_ptr().cast::<c_void>(),
                buffer.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let returned = returned as usize;
        if returned < 8 {
            return Ok(start_usn);
        }
        let next_usn = read_u64(buffer, 0) as i64;
        let mut name = Vec::with_capacity(256);
        parse_records(&buffer[8..returned], &mut name, &mut on_record);
        Ok(next_usn)
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `Volume::open` and is closed only here.
        unsafe { CloseHandle(self.handle) };
    }
}

fn read_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn read_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

const USN_RECORD_V2_HEADER: usize = 60;

/// Parses a packed run of `USN_RECORD_V2` structures.
fn parse_records(data: &[u8], name: &mut Vec<u16>, on_record: &mut impl FnMut(&Record<'_>)) {
    let mut offset = 0;
    while offset + USN_RECORD_V2_HEADER <= data.len() {
        let record_len = read_u32(data, offset) as usize;
        if record_len < USN_RECORD_V2_HEADER || offset + record_len > data.len() {
            break;
        }
        let record = &data[offset..offset + record_len];
        if read_u16(record, 4) == 2 {
            let name_len = read_u16(record, 56) as usize;
            let name_offset = read_u16(record, 58) as usize;
            if let Some(name_bytes) = record.get(name_offset..name_offset + name_len) {
                name.clear();
                name.extend(
                    name_bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&c| u16::from_le_bytes(c)),
                );
                on_record(&Record {
                    file_reference: read_u64(record, 8),
                    parent_reference: read_u64(record, 16),
                    usn: read_u64(record, 24) as i64,
                    reason: read_u32(record, 40),
                    attributes: read_u32(record, 52),
                    name: name.as_slice(),
                });
            }
        }
        offset += record_len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usn_record(file_ref: u64, parent_ref: u64, attrs: u32, reason: u32, name: &str) -> Vec<u8> {
        let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = (USN_RECORD_V2_HEADER + name.len()).next_multiple_of(8);
        let mut r = vec![0u8; len];
        r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[8..16].copy_from_slice(&file_ref.to_le_bytes());
        r[16..24].copy_from_slice(&parent_ref.to_le_bytes());
        r[24..32].copy_from_slice(&777i64.to_le_bytes());
        r[40..44].copy_from_slice(&reason.to_le_bytes());
        r[52..56].copy_from_slice(&attrs.to_le_bytes());
        r[56..58].copy_from_slice(&(name.len() as u16).to_le_bytes());
        r[58..60].copy_from_slice(&(USN_RECORD_V2_HEADER as u16).to_le_bytes());
        r[60..60 + name.len()].copy_from_slice(&name);
        r
    }

    #[test]
    fn parses_packed_records() {
        let mut data = usn_record(
            0x0003_0000_0000_0040,
            0x0005_0000_0000_0005,
            FILE_ATTRIBUTE_DIRECTORY,
            0,
            "Users",
        );
        data.extend(usn_record(
            0x0001_0000_0000_0041,
            0x0003_0000_0000_0040,
            FILE_ATTRIBUTE_HIDDEN,
            reason::FILE_CREATE,
            "ntuser.dat",
        ));
        let mut seen = Vec::new();
        parse_records(&data, &mut Vec::new(), &mut |r: &Record<'_>| {
            seen.push((
                r.record_number(),
                r.parent_record_number(),
                r.is_dir(),
                r.is_hidden_or_system(),
                r.reason,
                r.usn,
                String::from_utf16_lossy(r.name),
            ));
        });
        assert_eq!(
            seen,
            vec![
                (0x40, ROOT_RECORD, true, false, 0, 777, "Users".to_owned()),
                (
                    0x41,
                    0x40,
                    false,
                    true,
                    reason::FILE_CREATE,
                    777,
                    "ntuser.dat".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn stops_on_truncated_record() {
        let mut data = usn_record(0x41, 5, 0, 0, "a.txt");
        data.truncate(data.len() - 4);
        let mut count = 0;
        parse_records(&data, &mut Vec::new(), &mut |_: &Record<'_>| count += 1);
        assert_eq!(count, 0);
    }

    #[test]
    fn classifies_changes() {
        let kind = |reason: u32| {
            let mut kind = None;
            parse_records(
                &usn_record(0x41, 5, 0, reason, "a"),
                &mut Vec::new(),
                &mut |r: &Record<'_>| {
                    kind = Some(r.change_kind());
                },
            );
            kind.unwrap()
        };
        assert_eq!(kind(reason::FILE_CREATE), ChangeKind::Upsert);
        assert_eq!(kind(reason::RENAME_NEW_NAME), ChangeKind::Upsert);
        assert_eq!(kind(reason::BASIC_INFO_CHANGE), ChangeKind::Upsert);
        assert_eq!(kind(reason::RENAME_OLD_NAME), ChangeKind::Skip);
        assert_eq!(
            kind(reason::FILE_CREATE | reason::FILE_DELETE),
            ChangeKind::Delete
        );
        assert_eq!(
            kind(reason::RENAME_OLD_NAME | reason::FILE_DELETE),
            ChangeKind::Delete
        );
    }
}
