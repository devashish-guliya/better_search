//! Compact in-memory index of file and folder names.
//!
//! Data is stored as parallel arrays (one slot per entry) instead of one struct per file:
//! each entry costs a name id, a parent index and a flags byte. Names are UTF-8, stored
//! once in a shared buffer and shared between entries with the same name. Full paths are
//! never stored; they are rebuilt by walking parent links.
//!
//! The index can be updated in place from change journal events ([`Index::apply`]) and
//! saved to / loaded from disk (see [`snapshot`]).

pub mod snapshot;

use std::ops::Range;

use hashbrown::{DefaultHashBuilder, HashTable};
use std::hash::BuildHasher;

/// Parent value of volume root entries.
pub const NO_PARENT: u32 = u32::MAX;

/// Paths deeper than this are treated as corrupt (parent loops) and cut off.
const MAX_DEPTH: usize = 512;

/// Record ids above this are ignored; no real NTFS volume has that many MFT records.
const MAX_RECORD: u64 = 1 << 32;

pub mod flags {
    pub const DIR: u8 = 1;
    /// Hidden or system attribute.
    pub const HIDDEN: u8 = 1 << 1;
    pub const LOCATION_SHIFT: u8 = 2;
    pub const LOCATION_MASK: u8 = 0b11 << LOCATION_SHIFT;
    /// Removed entry, kept in place until the next [`crate::Index::compact`].
    pub const DELETED: u8 = 1 << 4;
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
///
/// Names are stored with ASCII letters lowercased so searches can compare bytes
/// directly. One bit per byte remembers which letters were uppercase, which restores
/// the original spelling for display at 1/8 of the cost of a second copy.
pub struct NameTable {
    folded: Vec<u8>,
    upper: Vec<u64>,
    /// `offsets[id]..offsets[id + 1]` is the byte range of name `id`.
    offsets: Vec<u32>,
}

impl Default for NameTable {
    fn default() -> Self {
        Self {
            folded: Vec::new(),
            upper: Vec::new(),
            offsets: vec![0],
        }
    }
}

impl NameTable {
    fn push(&mut self, name: &[u8]) -> u32 {
        let id = self.len() as u32;
        let start = self.folded.len();
        self.folded.extend(name.iter().map(u8::to_ascii_lowercase));
        self.upper.resize(self.folded.len().div_ceil(64), 0);
        for (i, &b) in name.iter().enumerate() {
            if b.is_ascii_uppercase() {
                let at = start + i;
                self.upper[at / 64] |= 1 << (at % 64);
            }
        }
        self.offsets.push(self.folded.len() as u32);
        id
    }

    /// Copies name `id` of another table, returning its id in this table.
    fn push_from(&mut self, other: &NameTable, id: u32) -> u32 {
        let new_id = self.len() as u32;
        let range = other.range(id);
        let start = self.folded.len();
        self.folded.extend_from_slice(&other.folded[range.clone()]);
        self.upper.resize(self.folded.len().div_ceil(64), 0);
        for (i, src) in range.enumerate() {
            if other.is_upper(src) {
                let at = start + i;
                self.upper[at / 64] |= 1 << (at % 64);
            }
        }
        self.offsets.push(self.folded.len() as u32);
        new_id
    }

    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Byte range of name `id` inside [`Self::folded_buffer`].
    #[inline]
    pub fn range(&self, id: u32) -> Range<usize> {
        let id = id as usize;
        self.offsets[id] as usize..self.offsets[id + 1] as usize
    }

    /// Name `id` with ASCII letters lowercased. This is what searches compare against.
    #[inline]
    pub fn folded(&self, id: u32) -> &[u8] {
        &self.folded[self.range(id)]
    }

    /// All lowercased names back to back, for scanning the whole table in one pass.
    pub fn folded_buffer(&self) -> &[u8] {
        &self.folded
    }

    /// Name `id` occupies `folded_buffer()[offsets()[id]..offsets()[id + 1]]`.
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }

    /// Whether the byte at `index` of [`Self::folded_buffer`] was an uppercase letter.
    #[inline]
    pub fn is_upper(&self, index: usize) -> bool {
        (self.upper[index / 64] >> (index % 64)) & 1 != 0
    }

    /// Appends the original spelling of name `id` to `out`.
    pub fn write_original(&self, id: u32, out: &mut String) {
        let range = self.range(id);
        let folded = std::str::from_utf8(&self.folded[range.clone()])
            .expect("names are only stored from valid UTF-8");
        for (i, c) in folded.char_indices() {
            if c.is_ascii_lowercase() && self.is_upper(range.start + i) {
                out.push(c.to_ascii_uppercase());
            } else {
                out.push(c);
            }
        }
    }

    pub fn get(&self, id: u32) -> String {
        let mut s = String::with_capacity(self.range(id).len());
        self.write_original(id, &mut s);
        s
    }

    fn matches_original(&self, id: u32, name: &[u8]) -> bool {
        let range = self.range(id);
        range.len() == name.len()
            && name.iter().zip(range).all(|(&b, i)| {
                self.folded[i] == b.to_ascii_lowercase()
                    && self.is_upper(i) == b.is_ascii_uppercase()
            })
    }

    fn heap_bytes(&self) -> usize {
        self.folded.capacity()
            + self.upper.capacity() * size_of::<u64>()
            + self.offsets.capacity() * size_of::<u32>()
    }

    fn shrink_to_fit(&mut self) {
        self.folded.shrink_to_fit();
        self.upper.shrink_to_fit();
        self.offsets.shrink_to_fit();
    }
}

/// Finds existing names so repeated names are stored once. Hashes the lowercased
/// bytes; exact case is checked on lookup, so `README.md` and `readme.md` stay distinct.
#[derive(Default)]
struct Interner {
    table: HashTable<u32>,
    hasher: DefaultHashBuilder,
    scratch: Vec<u8>,
}

impl Interner {
    fn intern(&mut self, names: &mut NameTable, name: &[u8]) -> u32 {
        self.scratch.clear();
        self.scratch.extend(name.iter().map(u8::to_ascii_lowercase));
        let hash = self.hasher.hash_one(&self.scratch[..]);
        if let Some(&id) = self
            .table
            .find(hash, |&id| names.matches_original(id, name))
        {
            return id;
        }
        let id = names.push(name);
        let hasher = &self.hasher;
        self.table
            .insert_unique(hash, id, |&id| hasher.hash_one(names.folded(id)));
        id
    }

    fn rebuild(names: &NameTable) -> Self {
        let mut interner = Self::default();
        interner.table.reserve(names.len(), |_| 0);
        for id in 0..names.len() as u32 {
            let hash = interner.hasher.hash_one(names.folded(id));
            let hasher = &interner.hasher;
            interner
                .table
                .insert_unique(hash, id, |&id| hasher.hash_one(names.folded(id)));
        }
        interner
    }

    fn heap_bytes(&self) -> usize {
        // One u32 slot plus one control byte per bucket.
        self.table.capacity() * (size_of::<u32>() + 1)
    }
}

/// Where the index last caught up with a volume's change journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncPoint {
    pub volume_serial: u32,
    pub journal_id: u64,
    pub next_usn: i64,
}

/// One indexed drive or folder tree.
pub struct Volume {
    pub label: String,
    /// Entry index of the volume root.
    pub root: u32,
    /// Source id of the root folder (5 on NTFS).
    pub root_record: u64,
    /// Present when the volume can be kept up to date from a change journal.
    pub sync: Option<SyncPoint>,
    /// Maps a source id (for NTFS, the MFT record number) to its entry index.
    record_lookup: Vec<u32>,
}

impl Volume {
    pub fn entry_for_record(&self, record: u64) -> Option<u32> {
        let entry = *self.record_lookup.get(usize::try_from(record).ok()?)?;
        (entry != NO_PARENT).then_some(entry)
    }

    fn set_record(&mut self, record: u64, entry: u32) {
        let at = record as usize;
        if at >= self.record_lookup.len() {
            self.record_lookup.resize(at + 1, NO_PARENT);
        }
        self.record_lookup[at] = entry;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryUsage {
    pub name_bytes: usize,
    pub entry_bytes: usize,
    pub lookup_bytes: usize,
    pub interner_bytes: usize,
}

impl MemoryUsage {
    pub fn total(&self) -> usize {
        self.name_bytes + self.entry_bytes + self.lookup_bytes + self.interner_bytes
    }
}

/// A file system change to apply to the index.
#[derive(Clone, Copy, Debug)]
pub enum Change<'a> {
    /// Create the entry, or update its name, parent and attributes.
    Upsert {
        record: u64,
        parent_record: u64,
        name: &'a str,
        is_dir: bool,
        hidden: bool,
    },
    Delete {
        record: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Created(u32),
    Updated(u32),
    Deleted(u32),
    Unchanged,
}

pub struct Index {
    names: NameTable,
    interner: Interner,
    name_ids: Vec<u32>,
    parents: Vec<u32>,
    flags: Vec<u8>,
    volumes: Vec<Volume>,
    deleted: usize,
    generation: u64,
    locations_dirty: bool,
    batch_changed: bool,
}

impl Index {
    /// Number of entry slots, including deleted ones awaiting compaction.
    pub fn len(&self) -> usize {
        self.name_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name_ids.is_empty()
    }

    /// Number of entries that are not deleted.
    pub fn live_len(&self) -> usize {
        self.len() - self.deleted
    }

    pub fn deleted_len(&self) -> usize {
        self.deleted
    }

    /// Changes whenever the index content changes, so cached search state can be dropped.
    pub fn generation(&self) -> u64 {
        self.generation
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

    pub fn set_sync(&mut self, volume: usize, sync: Option<SyncPoint>) {
        self.volumes[volume].sync = sync;
    }

    pub fn name(&self, entry: u32) -> String {
        self.names.get(self.name_ids[entry as usize])
    }

    fn name_folded(&self, entry: u32) -> &[u8] {
        self.names.folded(self.name_ids[entry as usize])
    }

    pub fn parent(&self, entry: u32) -> Option<u32> {
        let parent = self.parents[entry as usize];
        (parent != NO_PARENT).then_some(parent)
    }

    pub fn is_dir(&self, entry: u32) -> bool {
        self.flags[entry as usize] & flags::DIR != 0
    }

    pub fn is_deleted(&self, entry: u32) -> bool {
        self.flags[entry as usize] & flags::DELETED != 0
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
            self.names
                .write_original(self.name_ids[e as usize], &mut path);
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
            interner_bytes: self.interner.heap_bytes(),
        }
    }

    fn live_entry(&self, volume: usize, record: u64) -> Option<u32> {
        self.volumes[volume]
            .entry_for_record(record)
            .filter(|&e| !self.is_deleted(e))
    }

    /// Whether `ancestor` is `entry` or one of its parents.
    fn is_ancestor_or_self(&self, ancestor: u32, entry: u32) -> bool {
        let mut current = Some(entry);
        for _ in 0..MAX_DEPTH {
            match current {
                Some(e) if e == ancestor => return true,
                Some(e) => current = self.parent(e),
                None => return false,
            }
        }
        true
    }

    /// Applies one change. Call [`Self::end_batch`] after a group of changes.
    pub fn apply(&mut self, volume: usize, change: Change<'_>) -> Applied {
        match change {
            Change::Delete { record } => {
                let Some(entry) = self.live_entry(volume, record) else {
                    return Applied::Unchanged;
                };
                if entry == self.volumes[volume].root {
                    return Applied::Unchanged;
                }
                self.flags[entry as usize] |= flags::DELETED;
                self.volumes[volume].record_lookup[record as usize] = NO_PARENT;
                self.deleted += 1;
                self.batch_changed = true;
                Applied::Deleted(entry)
            }
            Change::Upsert {
                record,
                parent_record,
                name,
                is_dir,
                hidden,
            } => {
                let vol = &self.volumes[volume];
                if record == vol.root_record || record >= MAX_RECORD {
                    return Applied::Unchanged;
                }
                let root = vol.root;
                let mut parent = self.live_entry(volume, parent_record).unwrap_or(root);
                let mut basic = 0;
                if is_dir {
                    basic |= flags::DIR;
                }
                if hidden {
                    basic |= flags::HIDDEN;
                }

                let (entry, applied) = match self.live_entry(volume, record) {
                    Some(entry) => {
                        let e = entry as usize;
                        let same_name = self
                            .names
                            .matches_original(self.name_ids[e], name.as_bytes());
                        if parent != root && self.is_ancestor_or_self(entry, parent) {
                            parent = root;
                        }
                        let old_basic = self.flags[e] & (flags::DIR | flags::HIDDEN);
                        if same_name && self.parents[e] == parent && old_basic == basic {
                            return Applied::Unchanged;
                        }
                        let moved = !same_name || self.parents[e] != parent;
                        if !same_name {
                            self.name_ids[e] =
                                self.interner.intern(&mut self.names, name.as_bytes());
                        }
                        self.parents[e] = parent;
                        self.flags[e] = (self.flags[e] & !(flags::DIR | flags::HIDDEN)) | basic;
                        if moved && is_dir {
                            // Descendants may now be in a different location class.
                            self.locations_dirty = true;
                        }
                        (entry, Applied::Updated(entry))
                    }
                    None => {
                        let entry = self.name_ids.len() as u32;
                        let name_id = self.interner.intern(&mut self.names, name.as_bytes());
                        self.name_ids.push(name_id);
                        self.parents.push(parent);
                        self.flags.push(basic);
                        self.volumes[volume].set_record(record, entry);
                        (entry, Applied::Created(entry))
                    }
                };
                let inherited = Location::from_flags(self.flags[parent as usize]);
                let location = own_location(self, entry, inherited);
                let e = entry as usize;
                self.flags[e] = (self.flags[e] & !flags::LOCATION_MASK) | location.to_bits();
                self.batch_changed = true;
                applied
            }
        }
    }

    /// Finishes a group of changes: fixes up locations if folders moved and bumps
    /// the generation if anything changed.
    pub fn end_batch(&mut self) {
        if self.locations_dirty {
            assign_locations(self);
            self.locations_dirty = false;
        }
        if self.batch_changed {
            self.generation += 1;
            self.batch_changed = false;
        }
    }

    /// Drops deleted entries and names nothing refers to anymore.
    pub fn compact(&mut self) {
        let unused_names = !self.names.is_empty() && {
            let mut used = vec![false; self.names.len()];
            for (e, &id) in self.name_ids.iter().enumerate() {
                if self.flags[e] & flags::DELETED == 0 {
                    used[id as usize] = true;
                }
            }
            used.iter().any(|u| !u)
        };
        if self.deleted == 0 && !unused_names {
            return;
        }

        let mut remap = vec![NO_PARENT; self.len()];
        let mut name_remap = vec![NO_PARENT; self.names.len()];
        let mut names = NameTable::default();
        let live = self.live_len();
        let mut name_ids = Vec::with_capacity(live);
        let mut flags_out = Vec::with_capacity(live);
        for (e, new_index) in remap.iter_mut().enumerate() {
            if self.flags[e] & flags::DELETED != 0 {
                continue;
            }
            *new_index = name_ids.len() as u32;
            let old_name = self.name_ids[e] as usize;
            if name_remap[old_name] == NO_PARENT {
                name_remap[old_name] = names.push_from(&self.names, old_name as u32);
            }
            name_ids.push(name_remap[old_name]);
            flags_out.push(self.flags[e]);
        }
        let mut parents = Vec::with_capacity(live);
        for (e, &new_index) in remap.iter().enumerate() {
            if new_index == NO_PARENT {
                continue;
            }
            // Skip over deleted ancestors; volume roots are never deleted.
            let mut p = self.parents[e];
            let mut depth = 0;
            while p != NO_PARENT && remap[p as usize] == NO_PARENT && depth < MAX_DEPTH {
                p = self.parents[p as usize];
                depth += 1;
            }
            parents.push(if p == NO_PARENT {
                NO_PARENT
            } else {
                remap[p as usize]
            });
        }
        for volume in &mut self.volumes {
            volume.root = remap[volume.root as usize];
            for slot in &mut volume.record_lookup {
                if *slot != NO_PARENT {
                    *slot = remap[*slot as usize];
                }
            }
        }
        names.shrink_to_fit();
        self.interner = Interner::rebuild(&names);
        self.names = names;
        self.name_ids = name_ids;
        self.parents = parents;
        self.flags = flags_out;
        self.deleted = 0;
        self.generation += 1;
    }

    fn from_parts(
        names: NameTable,
        name_ids: Vec<u32>,
        parents: Vec<u32>,
        flags: Vec<u8>,
        volumes: Vec<Volume>,
    ) -> Self {
        let deleted = flags.iter().filter(|&&f| f & flags::DELETED != 0).count();
        Self {
            interner: Interner::rebuild(&names),
            names,
            name_ids,
            parents,
            flags,
            volumes,
            deleted,
            generation: 0,
            locations_dirty: false,
            batch_changed: false,
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
    interner: Interner,
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
            names: NameTable::default(),
            interner: Interner::default(),
            name_ids: Vec::new(),
            parents: Vec::new(),
            flags: Vec::new(),
            volumes: Vec::new(),
            pending: None,
            utf8_buf: String::new(),
        }
    }

    /// Starts a volume. `label` becomes the first path component (for example `C:`).
    /// Entries whose parent is `root_id` are placed directly under the volume root.
    pub fn begin_volume(&mut self, label: &str, root_id: u64) {
        assert!(self.pending.is_none(), "previous volume was not ended");
        let root_entry = self.name_ids.len() as u32;
        let name_id = self.interner.intern(&mut self.names, label.as_bytes());
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
        let pending = self.pending.as_mut().expect("push called outside a volume");
        if id == pending.root_id {
            return;
        }
        pending.source_ids.push(id);
        pending.source_parent_ids.push(parent_id);
        let name_id = self.interner.intern(&mut self.names, name.as_bytes());
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
        let max_reasonable = ((count as u64).saturating_mul(16) + (1 << 20)).min(MAX_RECORD);
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
            root_record: pending.root_id,
            sync: None,
            record_lookup: lookup,
        });
    }

    pub fn finish(mut self) -> Index {
        assert!(self.pending.is_none(), "last volume was not ended");
        self.names.shrink_to_fit();
        self.name_ids.shrink_to_fit();
        self.parents.shrink_to_fit();
        self.flags.shrink_to_fit();
        let mut index = Index {
            names: self.names,
            interner: self.interner,
            name_ids: self.name_ids,
            parents: self.parents,
            flags: self.flags,
            volumes: self.volumes,
            deleted: 0,
            generation: 0,
            locations_dirty: false,
            batch_changed: false,
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

// All lists are lowercase because names are compared in their lowercased form.
const NOISY_ANYWHERE: &[&[u8]] = &[
    b"node_modules",
    b".git",
    b"appdata",
    b"winsxs",
    b"__pycache__",
    b".cache",
    b".npm",
    b".cargo",
    b".rustup",
    b".gradle",
    b".m2",
    b".nuget",
];

const NOISY_AT_ROOT: &[&[u8]] = &[
    b"windows",
    b"programdata",
    b"windows.old",
    b"recovery",
    b"system volume information",
    b"msocache",
];

const USER_CONTENT: &[&[u8]] = &[
    b"desktop",
    b"documents",
    b"downloads",
    b"pictures",
    b"videos",
    b"music",
    b"onedrive",
];

fn own_location(index: &Index, entry: u32, inherited: Location) -> Location {
    let Some(parent) = index.parent(entry) else {
        return Location::Normal;
    };
    if inherited == Location::Noisy {
        return Location::Noisy;
    }
    let name = index.name_folded(entry);
    let at_root = index.parent(parent).is_none();
    // NTFS metadata files and folders ($Extend, $Recycle.Bin, ...) live at the root.
    if at_root && name.starts_with(b"$") {
        return Location::Noisy;
    }
    if index.is_dir(entry) {
        if NOISY_ANYWHERE.contains(&name) || (at_root && NOISY_AT_ROOT.contains(&name)) {
            return Location::Noisy;
        }
        // <root>\Users\<profile>\<content folder>
        if inherited == Location::Normal && USER_CONTENT.contains(&name) {
            let users_at_root = index.parent(parent).is_some_and(|u| {
                index.name_folded(u) == b"users"
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
    pub(crate) fn sample() -> Index {
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

    pub(crate) fn find(index: &Index, path: &str) -> u32 {
        try_find(index, path).unwrap_or_else(|| panic!("{path} not in index"))
    }

    fn try_find(index: &Index, path: &str) -> Option<u32> {
        (0..index.len() as u32).find(|&e| !index.is_deleted(e) && index.full_path(e) == path)
    }

    fn upsert(record: u64, parent_record: u64, name: &str, is_dir: bool) -> Change<'_> {
        Change::Upsert {
            record,
            parent_record,
            name,
            is_dir,
            hidden: false,
        }
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
    fn keeps_original_case_and_distinguishes_case_variants() {
        let mut b = IndexBuilder::new();
        b.begin_volume("X:", 0);
        b.push(1, 0, "README.md", false, false);
        b.push(2, 0, "readme.md", false, false);
        b.push(3, 0, "ReadMe.MD", false, false);
        b.push(4, 0, "README.md", false, false);
        b.end_volume();
        let index = b.finish();
        assert_eq!(index.names().len(), 4); // "X:" + three spellings
        assert_eq!(index.full_path(1), "X:\\README.md");
        assert_eq!(index.full_path(2), "X:\\readme.md");
        assert_eq!(index.full_path(3), "X:\\ReadMe.MD");
        assert_eq!(index.names().folded(index.name_ids()[3]), b"readme.md");
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
        let name: Vec<u16> = "Résumé.PDF".encode_utf16().collect();
        b.push_utf16(1, 0, &name, false, false);
        b.end_volume();
        let index = b.finish();
        assert_eq!(index.full_path(1), "X:\\Résumé.PDF");
    }

    #[test]
    fn creates_entries_with_location() {
        let mut index = sample();
        let g = index.generation();
        let applied = index.apply(0, upsert(50, 12, "Tax 2024.pdf", false));
        let Applied::Created(entry) = applied else {
            panic!("{applied:?}")
        };
        index.end_batch();
        assert_eq!(
            index.full_path(entry),
            "C:\\Users\\bob\\Documents\\Tax 2024.pdf"
        );
        assert_eq!(index.location(entry), Location::UserContent);
        assert!(index.generation() > g);
    }

    #[test]
    fn repeated_upsert_is_unchanged() {
        let mut index = sample();
        index.apply(0, upsert(50, 12, "a.txt", false));
        index.end_batch();
        let g = index.generation();
        assert_eq!(
            index.apply(0, upsert(50, 12, "a.txt", false)),
            Applied::Unchanged
        );
        index.end_batch();
        assert_eq!(index.generation(), g);
    }

    #[test]
    fn renames_and_moves() {
        let mut index = sample();
        // report.docx (13) renamed and moved into C:\code
        let applied = index.apply(0, upsert(13, 31, "Report Final.docx", false));
        assert!(matches!(applied, Applied::Updated(_)));
        index.end_batch();
        let entry = find(&index, "C:\\code\\Report Final.docx");
        assert_eq!(index.location(entry), Location::Normal);
        assert!(try_find(&index, "C:\\Users\\bob\\Documents\\report.docx").is_none());
    }

    #[test]
    fn moving_a_folder_updates_descendant_locations() {
        let mut index = sample();
        // Move C:\code\app into C:\Users\bob\Documents
        index.apply(0, upsert(33, 12, "app", true));
        index.end_batch();
        let entry = find(&index, "C:\\Users\\bob\\Documents\\app\\index.js");
        assert_eq!(index.location(entry), Location::UserContent);
        let nm = find(
            &index,
            "C:\\Users\\bob\\Documents\\app\\node_modules\\index.js",
        );
        assert_eq!(index.location(nm), Location::Noisy);
    }

    #[test]
    fn deletes_entries() {
        let mut index = sample();
        let entry = find(&index, "C:\\Windows\\notepad.exe");
        assert_eq!(
            index.apply(0, Change::Delete { record: 21 }),
            Applied::Deleted(entry)
        );
        assert_eq!(
            index.apply(0, Change::Delete { record: 21 }),
            Applied::Unchanged
        );
        index.end_batch();
        assert!(index.is_deleted(entry));
        assert_eq!(index.deleted_len(), 1);
        assert_eq!(index.volumes()[0].entry_for_record(21), None);
    }

    #[test]
    fn move_into_own_subfolder_is_rejected() {
        let mut index = sample();
        // Try to move C:\code (31) under C:\code\app (33): that would create a loop.
        index.apply(0, upsert(31, 33, "code", true));
        index.end_batch();
        find(&index, "C:\\code\\app\\index.js");
    }

    #[test]
    fn compaction_drops_deleted_entries_and_unused_names() {
        let mut index = sample();
        index.apply(0, Change::Delete { record: 21 });
        index.apply(0, Change::Delete { record: 40 });
        index.apply(0, upsert(50, 20, "calc.exe", false));
        index.end_batch();
        let names_before = index.names().len();
        index.compact();
        assert_eq!(index.deleted_len(), 0);
        assert_eq!(index.len(), 14);
        assert_eq!(index.names().len(), names_before - 2);
        let calc = find(&index, "C:\\Windows\\calc.exe");
        assert_eq!(index.volumes()[0].entry_for_record(50), Some(calc));
        assert_eq!(index.volumes()[0].entry_for_record(5), Some(0));
        find(&index, "C:\\code\\app\\node_modules\\index.js");
        // The interner still works after compaction.
        index.apply(0, upsert(51, 20, "calc.exe", false));
        index.end_batch();
        assert_eq!(index.names().len(), names_before - 2);
    }
}
