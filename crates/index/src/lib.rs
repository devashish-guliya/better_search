//! Compact in-memory index of file and folder names.
//!
//! Data is stored as parallel arrays (one slot per entry) instead of one struct per file:
//! each entry costs a name id, a parent index and a flags byte. Names are UTF-8, stored
//! once in a shared buffer and shared between entries with the same name. Full paths are
//! never stored; they are rebuilt by walking parent links.

use hashbrown::{DefaultHashBuilder, HashTable};
use std::hash::BuildHasher;

/// Parent value of volume root entries.
pub const NO_PARENT: u32 = u32::MAX;

/// Paths deeper than this are treated as corrupt (parent loops) and cut off.
const MAX_DEPTH: usize = 512;

pub mod flags {
    pub const DIR: u8 = 1;
    /// Hidden or system attribute.
    pub const HIDDEN: u8 = 1 << 1;
    pub const LOCATION_SHIFT: u8 = 2;
    pub const LOCATION_MASK: u8 = 0b11 << LOCATION_SHIFT;
}

/// Where an entry lives, used by ranking to boost or demote results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    Normal,
    /// Inside a user's Desktop, Documents, Downloads, Pictures, Videos, Music or OneDrive.
    UserContent,
    /// Inside system or clutter folders such as Windows, AppData or node_modules.
    Noisy,
}

impl Location {
    fn to_bits(self) -> u8 {
        let raw = match self {
            Location::Normal => 0,
            Location::UserContent => 1,
            Location::Noisy => 2,
        };
        raw << flags::LOCATION_SHIFT
    }

    pub fn from_flags(entry_flags: u8) -> Self {
        match (entry_flags & flags::LOCATION_MASK) >> flags::LOCATION_SHIFT {
            1 => Location::UserContent,
            2 => Location::Noisy,
            _ => Location::Normal,
        }
    }
}

/// Unique names packed into one buffer.
#[derive(Default)]
pub struct NameTable {
    bytes: Vec<u8>,
    /// `offsets[id]..offsets[id + 1]` is the byte range of name `id`.
    offsets: Vec<u32>,
}

impl NameTable {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            offsets: vec![0],
        }
    }

    fn push(&mut self, name: &[u8]) -> u32 {
        let id = (self.offsets.len() - 1) as u32;
        self.bytes.extend_from_slice(name);
        self.offsets.push(self.bytes.len() as u32);
        id
    }

    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Raw UTF-8 bytes of a name. This is what the search hot loop uses.
    #[inline]
    pub fn bytes(&self, id: u32) -> &[u8] {
        let id = id as usize;
        &self.bytes[self.offsets[id] as usize..self.offsets[id + 1] as usize]
    }

    /// All names back to back, for scanning the whole table in one pass.
    pub fn buffer(&self) -> &[u8] {
        &self.bytes
    }

    /// Name `id` occupies `buffer()[offsets()[id]..offsets()[id + 1]]`.
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }

    pub fn get(&self, id: u32) -> &str {
        std::str::from_utf8(self.bytes(id)).expect("names are only stored from &str")
    }

    fn heap_bytes(&self) -> usize {
        self.bytes.capacity() + self.offsets.capacity() * size_of::<u32>()
    }

    fn shrink_to_fit(&mut self) {
        self.bytes.shrink_to_fit();
        self.offsets.shrink_to_fit();
    }
}

/// One indexed drive or folder tree.
pub struct Volume {
    pub label: String,
    /// Entry index of the volume root.
    pub root: u32,
    /// Maps a source id (for NTFS, the MFT record number) to its entry index.
    /// Kept so later live updates from the change journal can find entries.
    record_lookup: Vec<u32>,
}

impl Volume {
    pub fn entry_for_record(&self, record: u64) -> Option<u32> {
        let entry = *self.record_lookup.get(usize::try_from(record).ok()?)?;
        (entry != NO_PARENT).then_some(entry)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryUsage {
    pub name_bytes: usize,
    pub entry_bytes: usize,
    pub lookup_bytes: usize,
}

impl MemoryUsage {
    pub fn total(&self) -> usize {
        self.name_bytes + self.entry_bytes + self.lookup_bytes
    }
}

pub struct Index {
    names: NameTable,
    name_ids: Vec<u32>,
    parents: Vec<u32>,
    flags: Vec<u8>,
    volumes: Vec<Volume>,
}

impl Index {
    pub fn len(&self) -> usize {
        self.name_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name_ids.is_empty()
    }

    pub fn names(&self) -> &NameTable {
        &self.names
    }

    pub fn name_ids(&self) -> &[u32] {
        &self.name_ids
    }

    pub fn flags(&self) -> &[u8] {
        &self.flags
    }

    pub fn volumes(&self) -> &[Volume] {
        &self.volumes
    }

    pub fn name(&self, entry: u32) -> &str {
        self.names.get(self.name_ids[entry as usize])
    }

    pub fn parent(&self, entry: u32) -> Option<u32> {
        let parent = self.parents[entry as usize];
        (parent != NO_PARENT).then_some(parent)
    }

    pub fn is_dir(&self, entry: u32) -> bool {
        self.flags[entry as usize] & flags::DIR != 0
    }

    pub fn location(&self, entry: u32) -> Location {
        Location::from_flags(self.flags[entry as usize])
    }

    /// Rebuilds the full path by walking parent links. Only call this for results
    /// that are actually shown.
    pub fn full_path(&self, entry: u32) -> String {
        let mut chain = Vec::with_capacity(16);
        let mut current = Some(entry);
        while let Some(e) = current {
            if chain.len() == MAX_DEPTH {
                break;
            }
            chain.push(e);
            current = self.parent(e);
        }
        let mut path = String::with_capacity(chain.len() * 16);
        for (i, &e) in chain.iter().rev().enumerate() {
            if i > 0 {
                path.push('\\');
            }
            path.push_str(self.name(e));
        }
        if chain.len() == 1 && self.parent(entry).is_none() {
            path.push('\\');
        }
        path
    }

    pub fn memory_usage(&self) -> MemoryUsage {
        MemoryUsage {
            name_bytes: self.names.heap_bytes(),
            entry_bytes: self.name_ids.capacity() * size_of::<u32>()
                + self.parents.capacity() * size_of::<u32>()
                + self.flags.capacity(),
            lookup_bytes: self
                .volumes
                .iter()
                .map(|v| v.record_lookup.capacity() * size_of::<u32>())
                .sum(),
        }
    }
}

struct PendingVolume {
    label: String,
    root_id: u64,
    root_entry: u32,
    source_ids: Vec<u64>,
    source_parent_ids: Vec<u64>,
}

/// Builds an [`Index`] one volume at a time.
///
/// Source ids are used directly as positions in a lookup table, so they must be small,
/// mostly dense numbers. NTFS MFT record numbers satisfy this.
pub struct IndexBuilder {
    names: NameTable,
    interner: HashTable<u32>,
    hasher: DefaultHashBuilder,
    name_ids: Vec<u32>,
    parents: Vec<u32>,
    flags: Vec<u8>,
    volumes: Vec<Volume>,
    pending: Option<PendingVolume>,
    utf8_buf: String,
}

impl Default for IndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self {
            names: NameTable::new(),
            interner: HashTable::new(),
            hasher: DefaultHashBuilder::default(),
            name_ids: Vec::new(),
            parents: Vec::new(),
            flags: Vec::new(),
            volumes: Vec::new(),
            pending: None,
            utf8_buf: String::new(),
        }
    }

    fn intern(&mut self, name: &[u8]) -> u32 {
        let hash = self.hasher.hash_one(name);
        let names = &mut self.names;
        if let Some(&id) = self.interner.find(hash, |&id| names.bytes(id) == name) {
            return id;
        }
        let id = names.push(name);
        let hasher = &self.hasher;
        self.interner
            .insert_unique(hash, id, |&id| hasher.hash_one(names.bytes(id)));
        id
    }

    /// Starts a volume. `label` becomes the first path component (for example `C:`).
    /// Entries whose parent is `root_id` are placed directly under the volume root.
    pub fn begin_volume(&mut self, label: &str, root_id: u64) {
        assert!(self.pending.is_none(), "previous volume was not ended");
        let root_entry = self.name_ids.len() as u32;
        let name_id = self.intern(label.as_bytes());
        self.name_ids.push(name_id);
        self.parents.push(NO_PARENT);
        self.flags.push(flags::DIR);
        self.pending = Some(PendingVolume {
            label: label.to_owned(),
            root_id,
            root_entry,
            source_ids: Vec::new(),
            source_parent_ids: Vec::new(),
        });
    }

    pub fn push(&mut self, id: u64, parent_id: u64, name: &str, is_dir: bool, hidden: bool) {
        let name_id = self.intern(name.as_bytes());
        let pending = self.pending.as_mut().expect("push called outside a volume");
        if id == pending.root_id {
            return;
        }
        pending.source_ids.push(id);
        pending.source_parent_ids.push(parent_id);
        self.name_ids.push(name_id);
        self.parents.push(NO_PARENT);
        let mut entry_flags = 0;
        if is_dir {
            entry_flags |= flags::DIR;
        }
        if hidden {
            entry_flags |= flags::HIDDEN;
        }
        self.flags.push(entry_flags);
    }

    pub fn push_utf16(
        &mut self,
        id: u64,
        parent_id: u64,
        name: &[u16],
        is_dir: bool,
        hidden: bool,
    ) {
        let mut buf = std::mem::take(&mut self.utf8_buf);
        buf.clear();
        buf.extend(char::decode_utf16(name.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')));
        self.push(id, parent_id, &buf, is_dir, hidden);
        self.utf8_buf = buf;
    }

    /// Resolves parent links for the current volume.
    pub fn end_volume(&mut self) {
        let pending = self
            .pending
            .take()
            .expect("end_volume called outside a volume");
        let first_entry = pending.root_entry as usize + 1;
        let count = pending.source_ids.len();

        // Ids far beyond the entry count would make the lookup table huge; such outliers
        // are left out and any children of them end up directly under the volume root.
        let max_reasonable = (count as u64).saturating_mul(16) + (1 << 20);
        let max_id = pending
            .source_ids
            .iter()
            .copied()
            .filter(|&id| id <= max_reasonable)
            .max()
            .unwrap_or(0)
            .max(pending.root_id.min(max_reasonable));
        let mut lookup = vec![NO_PARENT; max_id as usize + 1];
        for (k, &id) in pending.source_ids.iter().enumerate() {
            if let Some(slot) = lookup.get_mut(id as usize) {
                *slot = (first_entry + k) as u32;
            }
        }
        if let Some(slot) = lookup.get_mut(pending.root_id as usize) {
            *slot = pending.root_entry;
        }

        for (k, &parent_id) in pending.source_parent_ids.iter().enumerate() {
            let entry = (first_entry + k) as u32;
            let parent = lookup
                .get(parent_id as usize)
                .copied()
                .filter(|&p| p != NO_PARENT && p != entry)
                .unwrap_or(pending.root_entry);
            self.parents[entry as usize] = parent;
        }

        self.volumes.push(Volume {
            label: pending.label,
            root: pending.root_entry,
            record_lookup: lookup,
        });
    }

    pub fn finish(mut self) -> Index {
        assert!(self.pending.is_none(), "last volume was not ended");
        drop(std::mem::take(&mut self.interner));
        self.names.shrink_to_fit();
        self.name_ids.shrink_to_fit();
        self.parents.shrink_to_fit();
        self.flags.shrink_to_fit();
        let mut index = Index {
            names: self.names,
            name_ids: self.name_ids,
            parents: self.parents,
            flags: self.flags,
            volumes: self.volumes,
        };
        assign_locations(&mut index);
        index
    }
}

const UNRESOLVED: u8 = u8::MAX;

/// Computes the [`Location`] of every entry. Each entry inherits its parent's location
/// unless its own name changes it, so parents are resolved before children.
fn assign_locations(index: &mut Index) {
    let mut resolved = vec![UNRESOLVED; index.len()];
    let mut stack = Vec::with_capacity(64);
    for start in 0..index.len() as u32 {
        let mut current = Some(start);
        while let Some(e) = current {
            if resolved[e as usize] != UNRESOLVED || stack.len() == MAX_DEPTH {
                break;
            }
            stack.push(e);
            current = index.parent(e);
        }
        while let Some(e) = stack.pop() {
            let inherited = index
                .parent(e)
                .map(|p| resolved[p as usize])
                .filter(|&bits| bits != UNRESOLVED)
                .map_or(Location::Normal, Location::from_flags);
            let location = own_location(index, e, inherited);
            resolved[e as usize] = location.to_bits();
        }
    }
    for (entry_flags, bits) in index.flags.iter_mut().zip(resolved) {
        *entry_flags = (*entry_flags & !flags::LOCATION_MASK) | bits;
    }
}

const NOISY_ANYWHERE: &[&str] = &[
    "node_modules",
    ".git",
    "appdata",
    "winsxs",
    "__pycache__",
    ".cache",
    ".npm",
    ".cargo",
    ".rustup",
    ".gradle",
    ".m2",
    ".nuget",
];

const NOISY_AT_ROOT: &[&str] = &[
    "windows",
    "programdata",
    "windows.old",
    "recovery",
    "system volume information",
    "msocache",
];

const USER_CONTENT: &[&str] = &[
    "desktop",
    "documents",
    "downloads",
    "pictures",
    "videos",
    "music",
    "onedrive",
];

fn is_one_of(name: &str, list: &[&str]) -> bool {
    list.iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn own_location(index: &Index, entry: u32, inherited: Location) -> Location {
    let Some(parent) = index.parent(entry) else {
        return Location::Normal;
    };
    if inherited == Location::Noisy {
        return Location::Noisy;
    }
    let name = index.name(entry);
    let at_root = index.parent(parent).is_none();
    // NTFS metadata files and folders ($Extend, $Recycle.Bin, ...) live at the root.
    if at_root && name.starts_with('$') {
        return Location::Noisy;
    }
    if index.is_dir(entry) {
        if is_one_of(name, NOISY_ANYWHERE) || (at_root && is_one_of(name, NOISY_AT_ROOT)) {
            return Location::Noisy;
        }
        // <root>\Users\<profile>\<content folder>
        if inherited == Location::Normal && is_one_of(name, USER_CONTENT) {
            let users = index.parent(parent);
            let users_at_root = users.is_some_and(|u| {
                index.name(u).eq_ignore_ascii_case("users")
                    && index.parent(u).is_some_and(|r| index.parent(r).is_none())
            });
            if users_at_root {
                return Location::UserContent;
            }
        }
    }
    inherited
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a small tree:
    /// C:\Users\bob\Documents\report.docx
    /// C:\Users\bob\AppData\cache.bin
    /// C:\Windows\notepad.exe
    /// C:\code\app\node_modules\index.js
    /// C:\code\app\index.js
    /// C:\orphan.txt   (parent id 999 does not exist)
    fn sample() -> Index {
        let mut b = IndexBuilder::new();
        b.begin_volume("C:", 5);
        b.push(10, 5, "Users", true, false);
        b.push(11, 10, "bob", true, false);
        b.push(12, 11, "Documents", true, false);
        b.push(13, 12, "report.docx", false, false);
        b.push(14, 11, "AppData", true, true);
        b.push(15, 14, "cache.bin", false, false);
        b.push(20, 5, "Windows", true, false);
        b.push(21, 20, "notepad.exe", false, false);
        // Child listed before its parent, as NTFS enumeration order is by record number.
        b.push(30, 32, "index.js", false, false);
        b.push(31, 5, "code", true, false);
        b.push(32, 33, "node_modules", true, false);
        b.push(33, 31, "app", true, false);
        b.push(34, 33, "index.js", false, false);
        b.push(40, 999, "orphan.txt", false, false);
        b.end_volume();
        b.finish()
    }

    fn find(index: &Index, path: &str) -> u32 {
        (0..index.len() as u32)
            .find(|&e| index.full_path(e) == path)
            .unwrap_or_else(|| panic!("{path} not in index"))
    }

    #[test]
    fn rebuilds_full_paths() {
        let index = sample();
        assert_eq!(index.full_path(0), "C:\\");
        find(&index, "C:\\Users\\bob\\Documents\\report.docx");
        find(&index, "C:\\code\\app\\node_modules\\index.js");
        find(&index, "C:\\code\\app\\index.js");
    }

    #[test]
    fn unknown_parent_goes_under_root() {
        let index = sample();
        find(&index, "C:\\orphan.txt");
    }

    #[test]
    fn repeated_names_are_stored_once() {
        let index = sample();
        // 15 entries (root + 14), but "index.js" appears twice.
        assert_eq!(index.len(), 15);
        assert_eq!(index.names().len(), 14);
    }

    #[test]
    fn assigns_locations() {
        let index = sample();
        let loc = |p: &str| index.location(find(&index, p));
        assert_eq!(
            loc("C:\\Users\\bob\\Documents\\report.docx"),
            Location::UserContent
        );
        assert_eq!(loc("C:\\Users\\bob\\AppData\\cache.bin"), Location::Noisy);
        assert_eq!(loc("C:\\Windows\\notepad.exe"), Location::Noisy);
        assert_eq!(
            loc("C:\\code\\app\\node_modules\\index.js"),
            Location::Noisy
        );
        assert_eq!(loc("C:\\code\\app\\index.js"), Location::Normal);
    }

    #[test]
    fn record_lookup_finds_entries() {
        let index = sample();
        let volume = &index.volumes()[0];
        let entry = volume.entry_for_record(21).unwrap();
        assert_eq!(index.full_path(entry), "C:\\Windows\\notepad.exe");
        assert_eq!(volume.entry_for_record(5), Some(0));
        assert_eq!(volume.entry_for_record(999), None);
    }

    #[test]
    fn parent_loops_do_not_hang() {
        let mut b = IndexBuilder::new();
        b.begin_volume("X:", 0);
        b.push(1, 2, "a", true, false);
        b.push(2, 1, "b", true, false);
        b.end_volume();
        let index = b.finish();
        assert!(index.full_path(1).len() < 4 * MAX_DEPTH);
    }

    #[test]
    fn decodes_utf16_names() {
        let mut b = IndexBuilder::new();
        b.begin_volume("X:", 0);
        let name: Vec<u16> = "Résumé.pdf".encode_utf16().collect();
        b.push_utf16(1, 0, &name, false, false);
        b.end_volume();
        let index = b.finish();
        assert_eq!(index.full_path(1), "X:\\Résumé.pdf");
    }
}
