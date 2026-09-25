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

use hashbrown::{DefaultHashBuilder, HashMap, HashSet, HashTable};
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
    /// Folder whose contents are left out of the index (see [`crate::Index::skip_clutter`]).
    pub const SKIPPED: u8 = 1 << 5;
}

/// Version of the clutter rules in [`Index::skip_clutter`]. Bump it when the rules
/// change so saved indexes built with the old rules are rebuilt.
pub const SKIP_RULES_VERSION: u32 = 2;

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

    /// Makes room for one more name of `bytes` bytes without doubling the buffers.
    fn reserve_small(&mut self, bytes: usize) {
        reserve_small(&mut self.folded, bytes);
        reserve_small(&mut self.upper, bytes / 64 + 2);
        reserve_small(&mut self.offsets, 1);
    }
}

/// Grows `v` by a small fraction instead of the default doubling. Live updates add a
/// few entries at a time to arrays holding millions, and doubling would briefly need
/// twice the memory and then keep it.
fn reserve_small<T>(v: &mut Vec<T>, additional: usize) {
    if v.capacity() - v.len() < additional {
        v.reserve_exact(additional.max(v.len() / 64 + 1024));
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
    /// Finds the entry of a source id (for NTFS, the MFT record number).
    records: RecordMap,
}

impl Volume {
    /// Entry for `record`. May be an entry marked deleted and not yet compacted away.
    pub fn entry_for_record(&self, record: u64) -> Option<u32> {
        if record == self.root_record {
            return Some(self.root);
        }
        self.records.get(u32::try_from(record).ok()?)
    }
}

/// Maps record numbers to entries using 4 bytes per indexed entry.
///
/// A volume's entries are stored in record order right after its root, so the entry of
/// `sorted[i]` is simply `first_entry + i` and a binary search finds it. Only indexed
/// records are listed, which matters because most records on a typical drive belong to
/// skipped clutter or are unused. Entries created after the last compaction are kept in
/// a small hash map until the next compaction sorts them in.
#[derive(Default)]
struct RecordMap {
    first_entry: u32,
    sorted: Vec<u32>,
    recent: hashbrown::HashMap<u32, u32>,
}

impl RecordMap {
    fn get(&self, record: u32) -> Option<u32> {
        if let Some(&entry) = self.recent.get(&record) {
            return Some(entry);
        }
        let i = self.sorted.binary_search(&record).ok()?;
        Some(self.first_entry + i as u32)
    }

    fn heap_bytes(&self) -> usize {
        self.sorted.capacity() * size_of::<u32>()
            + self.recent.capacity() * (2 * size_of::<u32>() + 1)
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
    /// Renames since the last compaction; each may leave an unused name behind.
    renames: usize,
    /// [`SKIP_RULES_VERSION`] if clutter folders were skipped, 0 if everything is indexed.
    skip_rules: u32,
    generation: u64,
    /// Bumped when a change could alter which user owns an entry. Compaction bumps it
    /// too, because renumbering invalidates any map built from entry indices.
    users_epoch: u64,
    locations_dirty: bool,
    /// A folder left the index, so entries below it must go too.
    orphans_possible: bool,
    /// Folders whose contents gained a project marker or a project-rule folder in this
    /// batch; their rules are re-checked in [`Self::end_batch`].
    marker_parents: Vec<u32>,
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

    /// [`SKIP_RULES_VERSION`] if clutter folders were skipped, 0 if everything is indexed.
    pub fn skip_rules(&self) -> u32 {
        self.skip_rules
    }

    /// Whether enough deleted entries, unused names and not-yet-sorted records have
    /// built up to be worth a compaction (about 2% of the index).
    pub fn needs_compaction(&self) -> bool {
        let recent: usize = self.volumes.iter().map(|v| v.records.recent.len()).sum();
        (self.deleted + self.renames + recent) * 50 >= self.live_len().max(1000)
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

    /// A `<drive>:\Users` folder, which holds one private folder per user.
    fn is_users_folder(&self, entry: u32) -> bool {
        self.name_folded(entry) == b"users"
            && self.parent(entry).is_some_and(|p| self.parent(p).is_none())
    }

    /// A users folder itself, or a profile folder directly inside one: changing these
    /// changes which user a result belongs to, even when no ancestor moved.
    fn users_direct(&self, entry: u32) -> bool {
        self.is_users_folder(entry) || self.parent(entry).is_some_and(|p| self.is_users_folder(p))
    }

    /// The profile folder `entry` belongs to, if it is inside a users folder. The
    /// users folder itself and everything outside have no owner.
    fn users_owner(&self, entry: u32) -> Option<u32> {
        let mut current = Some(entry);
        for _ in 0..MAX_DEPTH {
            let e = current?;
            if self.is_users_folder(e) {
                return None;
            }
            if self.parent(e).is_some_and(|p| self.is_users_folder(p)) {
                return Some(e);
            }
            current = self.parent(e);
        }
        None
    }

    /// Changes whenever something changed that affects which user's profile a result
    /// belongs to, or when entries were renumbered by a compaction. A per-user privacy
    /// map built earlier can be reused while this is unchanged.
    pub fn users_epoch(&self) -> u64 {
        self.users_epoch
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
            lookup_bytes: self.volumes.iter().map(|v| v.records.heap_bytes()).sum(),
            interner_bytes: self.interner.heap_bytes(),
        }
    }

    fn add_name(&mut self, name: &str) -> u32 {
        self.names.reserve_small(name.len());
        self.interner.intern(&mut self.names, name.as_bytes())
    }

    /// The entry for `parent_record` if new children of it belong in the index.
    /// `None` means the parent is not indexed (inside a skipped folder, or unknown) or
    /// is a skipped folder itself.
    fn indexed_parent(&self, volume: usize, parent_record: u64) -> Option<u32> {
        self.live_entry(volume, parent_record)
            .filter(|&p| self.flags[p as usize] & flags::SKIPPED == 0)
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
                if self.users_direct(entry) {
                    self.users_epoch += 1;
                }
                self.remove_entry(volume, record, entry);
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
                let existing = self.live_entry(volume, record);
                // Children of folders that are not indexed are not indexed either. This
                // is what keeps new files inside skipped folders out, since the folders
                // below a skipped folder are not indexed. Windows always reports a folder
                // before the files in it, so a known folder is never missed this way.
                let Some(mut parent) = self.indexed_parent(volume, parent_record) else {
                    return match existing {
                        // Moved into a skipped folder: it leaves the index.
                        Some(entry) => {
                            self.remove_entry(volume, record, entry);
                            Applied::Deleted(entry)
                        }
                        None => Applied::Unchanged,
                    };
                };
                let mut basic = 0;
                if is_dir {
                    basic |= flags::DIR;
                }
                if hidden {
                    basic |= flags::HIDDEN;
                }
                let owner_before = existing.and_then(|entry| self.users_owner(entry));

                let (entry, applied, placed) = match existing {
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
                            self.name_ids[e] = self.add_name(name);
                            self.renames += 1;
                        }
                        self.parents[e] = parent;
                        self.flags[e] = (self.flags[e] & !(flags::DIR | flags::HIDDEN)) | basic;
                        if moved && is_dir {
                            // Descendants may now be in a different location class.
                            self.locations_dirty = true;
                        }
                        (entry, Applied::Updated(entry), moved)
                    }
                    None => {
                        let entry = self.name_ids.len() as u32;
                        reserve_small(&mut self.name_ids, 1);
                        reserve_small(&mut self.parents, 1);
                        reserve_small(&mut self.flags, 1);
                        let name_id = self.add_name(name);
                        self.name_ids.push(name_id);
                        self.parents.push(parent);
                        self.flags.push(basic);
                        self.volumes[volume]
                            .records
                            .recent
                            .insert(record as u32, entry);
                        (entry, Applied::Created(entry), true)
                    }
                };
                let inherited = Location::from_flags(self.flags[parent as usize]);
                let location = own_location(self, entry, inherited);
                let e = entry as usize;
                self.flags[e] = (self.flags[e] & !flags::LOCATION_MASK) | location.to_bits();
                // Only new entries, renames and moves can change which rules apply.
                if placed && self.skip_rules != 0 {
                    if self.flags[e] & flags::SKIPPED == 0 && skip_rule(self, entry, 0, 0).is_some()
                    {
                        self.flags[e] |= flags::SKIPPED;
                        if matches!(applied, Applied::Updated(_)) {
                            self.orphans_possible = true;
                        }
                    }
                    // Rules that depend on folder contents are checked in end_batch.
                    if marker_bits(self, entry) != 0
                        || (is_dir && is_project_rule_name(self.name_folded(entry)))
                    {
                        self.marker_parents.push(parent);
                    }
                }
                self.batch_changed = true;
                // Profile ownership changes only when an entry enters, leaves or moves
                // between users folders, or when a users folder or a profile folder
                // itself is created or changed. Everything else is picked up by
                // extending a map that is already built.
                if self.users_direct(entry)
                    || existing.is_some_and(|_| owner_before != self.users_owner(entry))
                {
                    self.users_epoch += 1;
                }
                applied
            }
        }
    }

    /// Finishes a group of changes: fixes up locations if folders moved and bumps
    /// the generation if anything changed.
    pub fn end_batch(&mut self) {
        if !self.marker_parents.is_empty() {
            self.apply_marker_rules();
        }
        if self.orphans_possible {
            self.remove_orphans();
            self.orphans_possible = false;
        }
        if self.locations_dirty {
            assign_locations(self);
            self.locations_dirty = false;
        }
        if self.batch_changed {
            self.generation += 1;
            self.batch_changed = false;
        }
    }

    fn remove_entry(&mut self, volume: usize, record: u64, entry: u32) {
        let e = entry as usize;
        self.flags[e] |= flags::DELETED;
        if let Ok(record) = u32::try_from(record) {
            self.volumes[volume].records.recent.remove(&record);
        }
        self.deleted += 1;
        if self.flags[e] & flags::DIR != 0 {
            self.orphans_possible = true;
        }
        self.batch_changed = true;
    }

    /// Marks entries below deleted or skipped folders as deleted. Windows deletes the
    /// contents of a folder before the folder, but a folder moved out of the index takes
    /// its contents along, and a folder that becomes skipped loses its contents.
    fn remove_orphans(&mut self) {
        const UNKNOWN: u8 = 0;
        const ALIVE: u8 = 1;
        const DEAD: u8 = 2;
        let mut state = vec![UNKNOWN; self.len()];
        let mut stack = Vec::with_capacity(64);
        for start in 0..self.len() as u32 {
            let mut current = Some(start);
            while let Some(e) = current {
                if state[e as usize] != UNKNOWN || stack.len() == MAX_DEPTH {
                    break;
                }
                stack.push(e);
                current = self.parent(e);
            }
            while let Some(e) = stack.pop() {
                let dead = self.is_deleted(e)
                    || self.parent(e).is_some_and(|p| {
                        state[p as usize] == DEAD || self.flags[p as usize] & flags::SKIPPED != 0
                    });
                state[e as usize] = if dead { DEAD } else { ALIVE };
            }
        }
        let mut removed = 0;
        for (e, &s) in state.iter().enumerate() {
            if s == DEAD && self.flags[e] & flags::DELETED == 0 {
                self.flags[e] |= flags::DELETED;
                removed += 1;
            }
        }
        if removed > 0 {
            self.deleted += removed;
            let flags = &self.flags;
            for v in &mut self.volumes {
                v.records
                    .recent
                    .retain(|_, e| flags[*e as usize] & flags::DELETED == 0);
            }
        }
    }

    /// Drops deleted entries and unused names in place. See [`Self::compacted`].
    pub fn compact(&mut self) {
        *self = self.compacted();
    }

    /// Builds a cleaned-up copy: deleted entries and unused names are dropped, and each
    /// volume's entries are put back in record order so the record lookup is a plain
    /// sorted list again. Only needs read access, so searches can keep using this index
    /// while the copy is built.
    pub fn compacted(&self) -> Index {
        // Old entry numbers in their new order: per volume, the root, then entries by
        // record number.
        let live = self.live_len();
        let mut order: Vec<u32> = Vec::with_capacity(live);
        let mut volume_layout = Vec::with_capacity(self.volumes.len());
        for v in &self.volumes {
            let root = order.len() as u32;
            order.push(v.root);
            let first_entry = order.len() as u32;
            let alive = |e: u32| self.flags[e as usize] & flags::DELETED == 0 && e != v.root;
            let mut recent: Vec<(u32, u32)> = v
                .records
                .recent
                .iter()
                .map(|(&r, &e)| (r, e))
                .filter(|&(_, e)| alive(e))
                .collect();
            recent.sort_unstable();
            let mut sorted = Vec::with_capacity(v.records.sorted.len() + recent.len());
            let mut recent = recent.into_iter().peekable();
            for (i, &record) in v.records.sorted.iter().enumerate() {
                while let Some(&(r, e)) = recent.peek()
                    && r <= record
                {
                    recent.next();
                    order.push(e);
                    sorted.push(r);
                }
                // A record that was reused for a newer entry: the newer one wins.
                if sorted.last() == Some(&record) {
                    continue;
                }
                let e = v.records.first_entry + i as u32;
                if alive(e) {
                    order.push(e);
                    sorted.push(record);
                }
            }
            for (r, e) in recent {
                order.push(e);
                sorted.push(r);
            }
            sorted.shrink_to_fit();
            volume_layout.push((root, first_entry, sorted));
        }

        let mut remap = vec![NO_PARENT; self.len()];
        for (new, &old) in order.iter().enumerate() {
            remap[old as usize] = new as u32;
        }
        let mut names = NameTable::default();
        let mut interner = Interner::default();
        let mut name_remap = vec![NO_PARENT; self.names.len()];
        let mut original = String::new();
        let mut name_ids = Vec::with_capacity(order.len());
        let mut flags_out = Vec::with_capacity(order.len());
        let mut parents = Vec::with_capacity(order.len());
        for &old in &order {
            let old_name = self.name_ids[old as usize] as usize;
            if name_remap[old_name] == NO_PARENT {
                original.clear();
                self.names.write_original(old_name as u32, &mut original);
                name_remap[old_name] = interner.intern(&mut names, original.as_bytes());
            }
            name_ids.push(name_remap[old_name]);
            flags_out.push(self.flags[old as usize]);
            // Skip over ancestors that were dropped; volume roots are always kept.
            let mut p = self.parents[old as usize];
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
        names.shrink_to_fit();
        let volumes = self
            .volumes
            .iter()
            .zip(volume_layout)
            .map(|(v, (root, first_entry, sorted))| Volume {
                label: v.label.clone(),
                root,
                root_record: v.root_record,
                sync: v.sync,
                records: RecordMap {
                    first_entry,
                    sorted,
                    recent: Default::default(),
                },
            })
            .collect();
        Index {
            names,
            interner,
            name_ids,
            parents,
            flags: flags_out,
            volumes,
            deleted: 0,
            renames: 0,
            skip_rules: self.skip_rules,
            generation: self.generation + 1,
            // Entry indices change in a compaction, so maps built from them are void.
            users_epoch: self.users_epoch + 1,
            locations_dirty: self.locations_dirty,
            orphans_possible: false,
            marker_parents: Vec::new(),
            batch_changed: false,
        }
    }

    /// Leaves out the contents of clutter folders such as `node_modules`, `.git`,
    /// caches, temp folders and Windows component stores. The folders themselves stay
    /// searchable. Files created inside them later are left out too.
    pub fn skip_clutter(&mut self) -> SkipReport {
        const UNKNOWN: u8 = 0;
        const KEPT: u8 = 1;
        const INSIDE: u8 = 2;
        self.skip_rules = SKIP_RULES_VERSION;
        let markers = self.markers_of(|_| true);
        let bits = |e: u32| markers.get(&e).copied().unwrap_or(0);
        // For skipped folders and everything inside them: the rule that applied.
        let mut rule_of = vec![0u8; self.len()];
        let mut state = vec![UNKNOWN; self.len()];
        let mut stack = Vec::with_capacity(64);
        for start in 0..self.len() as u32 {
            let mut current = Some(start);
            while let Some(e) = current {
                if state[e as usize] != UNKNOWN || stack.len() == MAX_DEPTH {
                    break;
                }
                stack.push(e);
                current = self.parent(e);
            }
            while let Some(e) = stack.pop() {
                let inside = self.parent(e).filter(|&p| {
                    state[p as usize] == INSIDE || self.flags[p as usize] & flags::SKIPPED != 0
                });
                state[e as usize] = match inside {
                    Some(p) => {
                        rule_of[e as usize] = rule_of[p as usize];
                        INSIDE
                    }
                    None => {
                        let siblings = self.parent(e).map_or(0, bits);
                        if let Some(rule) = skip_rule(self, e, bits(e), siblings) {
                            self.flags[e as usize] |= flags::SKIPPED;
                            rule_of[e as usize] = rule as u8;
                        }
                        KEPT
                    }
                };
            }
        }
        drop(markers);
        let mut removed = 0;
        let mut by_rule = vec![0usize; RULE_COUNT];
        for (e, &s) in state.iter().enumerate() {
            if s == INSIDE && self.flags[e] & flags::DELETED == 0 {
                self.flags[e] |= flags::DELETED;
                by_rule[rule_of[e] as usize] += 1;
                removed += 1;
            }
        }
        drop(rule_of);
        drop(state);
        self.deleted += removed;
        self.compact();
        let mut by_rule: Vec<(String, usize)> = by_rule
            .into_iter()
            .enumerate()
            .filter(|&(_, n)| n > 0)
            .map(|(rule, n)| (rule_name(rule), n))
            .collect();
        by_rule.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        SkipReport { removed, by_rule }
    }

    /// Marker bits per folder, for folders where `wanted(folder)` is true and that
    /// directly contain at least one marker.
    fn markers_of(&self, wanted: impl Fn(u32) -> bool) -> HashMap<u32, u16> {
        let mut markers = HashMap::new();
        for e in 0..self.len() as u32 {
            let parent = self.parents[e as usize];
            if parent == NO_PARENT || self.is_deleted(e) || !wanted(parent) {
                continue;
            }
            let bits = marker_bits(self, e);
            if bits != 0 {
                *markers.entry(parent).or_insert(0) |= bits;
            }
        }
        markers
    }

    /// Re-checks project and self-labelled folder rules for folders whose contents
    /// changed in this batch. A marker such as `Cargo.toml` may arrive after the
    /// `target` folder next to it, or a folder may get the `CACHEDIR.TAG` label later.
    fn apply_marker_rules(&mut self) {
        let pending: HashSet<u32> = std::mem::take(&mut self.marker_parents)
            .into_iter()
            .filter(|&p| !self.is_deleted(p) && self.flags[p as usize] & flags::SKIPPED == 0)
            .collect();
        if pending.is_empty() {
            return;
        }
        let markers = self.markers_of(|p| pending.contains(&p));
        let bits = |e: u32| markers.get(&e).copied().unwrap_or(0);
        let mut to_skip: Vec<u32> = markers
            .iter()
            .filter(|&(_, &b)| b & marker::SELF_LABELS != 0)
            .map(|(&p, _)| p)
            .collect();
        for e in 0..self.len() as u32 {
            let parent = self.parents[e as usize];
            if parent != NO_PARENT
                && pending.contains(&parent)
                && self.flags[e as usize] & (flags::DELETED | flags::SKIPPED) == 0
                && self.is_dir(e)
                && is_project_rule_name(self.name_folded(e))
            {
                to_skip.push(e);
            }
        }
        for e in to_skip {
            let siblings = self.parent(e).map_or(0, bits);
            if self.flags[e as usize] & flags::SKIPPED == 0
                && skip_rule(self, e, bits(e), siblings).is_some()
            {
                self.flags[e as usize] |= flags::SKIPPED;
                self.orphans_possible = true;
                self.batch_changed = true;
            }
        }
    }

    fn from_parts(parts: Parts) -> Self {
        let deleted = parts
            .flags
            .iter()
            .filter(|&&f| f & flags::DELETED != 0)
            .count();
        Self {
            interner: Interner::rebuild(&parts.names),
            names: parts.names,
            name_ids: parts.name_ids,
            parents: parts.parents,
            flags: parts.flags,
            volumes: parts.volumes,
            deleted,
            renames: 0,
            skip_rules: parts.skip_rules,
            generation: 0,
            users_epoch: 0,
            locations_dirty: false,
            orphans_possible: false,
            marker_parents: Vec::new(),
            batch_changed: false,
        }
    }
}

/// What [`Index::skip_clutter`] removed.
#[derive(Debug)]
pub struct SkipReport {
    pub removed: usize,
    /// Entries removed per folder rule, largest first.
    pub by_rule: Vec<(String, usize)>,
}

/// Everything a snapshot stores.
struct Parts {
    names: NameTable,
    name_ids: Vec<u32>,
    parents: Vec<u32>,
    flags: Vec<u8>,
    volumes: Vec<Volume>,
    skip_rules: u32,
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

        drop(lookup);
        let first_entry = first_entry as u32;
        let in_order = pending.source_ids.windows(2).all(|w| w[0] < w[1])
            && pending.source_ids.last().is_none_or(|&id| id < MAX_RECORD);
        let records = if in_order {
            // NTFS lists records in increasing order, so this is the normal case.
            RecordMap {
                first_entry,
                sorted: pending.source_ids.iter().map(|&id| id as u32).collect(),
                recent: Default::default(),
            }
        } else {
            // Sorted by the compaction in `finish`.
            RecordMap {
                first_entry,
                sorted: Vec::new(),
                recent: pending
                    .source_ids
                    .iter()
                    .enumerate()
                    .filter(|&(_, &id)| id < MAX_RECORD)
                    .map(|(k, &id)| (id as u32, first_entry + k as u32))
                    .collect(),
            }
        };
        self.volumes.push(Volume {
            label: pending.label,
            root: pending.root_entry,
            root_record: pending.root_id,
            sync: None,
            records,
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
            renames: 0,
            skip_rules: 0,
            generation: 0,
            users_epoch: 0,
            locations_dirty: false,
            orphans_possible: false,
            marker_parents: Vec::new(),
            batch_changed: false,
        };
        assign_locations(&mut index);
        if index.volumes.iter().any(|v| !v.records.recent.is_empty()) {
            index.compact();
        }
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

// Folders whose contents are never indexed when clutter is skipped. Only names that
// tools use and people do not.
const SKIP_ANYWHERE: &[&[u8]] = &[
    // Version control
    b".git",
    b".svn",
    b".hg",
    b".bzr",
    // JavaScript
    b"node_modules",
    b"bower_components",
    b"jspm_packages",
    b".npm",
    b".yarn",
    b".pnpm-store",
    b".next",
    b".nuxt",
    b".svelte-kit",
    b".angular",
    b".parcel-cache",
    b".turbo",
    b".expo",
    // Python
    b"__pycache__",
    b"site-packages",
    b".venv",
    b".tox",
    b".nox",
    b".pytest_cache",
    b".mypy_cache",
    b".ruff_cache",
    b".ipynb_checkpoints",
    b".conda",
    // JVM and Android
    b".gradle",
    b".m2",
    b".ivy2",
    b".android",
    b".cxx",
    b".externalnativebuild",
    // .NET and Visual Studio
    b".nuget",
    b".vs",
    // Rust, Dart and Flutter, Haskell
    b".rustup",
    b".cargo",
    b".dart_tool",
    b".pub-cache",
    b".stack-work",
    // C and C++
    b"cmakefiles",
    b"vcpkg_installed",
    // Infrastructure tools
    b".terraform",
    b".serverless",
    // General caches
    b".cache",
];

// Project marker bits: which kinds of project a folder holds, judged by the files
// directly inside it.
mod marker {
    pub const RUST: u16 = 1;
    pub const DOTNET: u16 = 1 << 1;
    pub const GRADLE: u16 = 1 << 2;
    pub const CMAKE: u16 = 1 << 3;
    pub const NODE: u16 = 1 << 4;
    pub const COMPOSER: u16 = 1 << 5;
    pub const GO: u16 = 1 << 6;
    pub const FLUTTER: u16 = 1 << 7;
    pub const PODS: u16 = 1 << 8;
    pub const UNREAL: u16 = 1 << 9;
    pub const UNITY_ASSETS: u16 = 1 << 10;
    pub const UNITY_SETTINGS: u16 = 1 << 11;
    /// Derived: both Unity folders are present (see [`super::project_kinds`]).
    pub const UNITY: u16 = 1 << 12;
    // The folder labels itself as disposable.
    pub const CACHEDIR_TAG: u16 = 1 << 13;
    pub const PYVENV: u16 = 1 << 14;
    pub const CMAKE_CACHE: u16 = 1 << 15;
    pub const SELF_LABELS: u16 = CACHEDIR_TAG | PYVENV | CMAKE_CACHE;
}

/// What `entry` says about the folder it is in.
fn marker_bits(index: &Index, entry: u32) -> u16 {
    let name = index.name_folded(entry);
    if index.is_dir(entry) {
        return match name {
            b"assets" => marker::UNITY_ASSETS,
            b"projectsettings" => marker::UNITY_SETTINGS,
            _ => 0,
        };
    }
    match name {
        b"cargo.toml" => marker::RUST,
        b"build.gradle" | b"build.gradle.kts" | b"settings.gradle" | b"settings.gradle.kts" => {
            marker::GRADLE
        }
        b"cmakelists.txt" => marker::CMAKE,
        b"package.json" => marker::NODE,
        b"composer.json" => marker::COMPOSER,
        b"go.mod" => marker::GO,
        b"pubspec.yaml" => marker::FLUTTER,
        b"podfile" => marker::PODS,
        b"cachedir.tag" => marker::CACHEDIR_TAG,
        b"pyvenv.cfg" => marker::PYVENV,
        b"cmakecache.txt" => marker::CMAKE_CACHE,
        _ if [&b".csproj"[..], b".vbproj", b".fsproj", b".sln"]
            .iter()
            .any(|ext| name.ends_with(ext)) =>
        {
            marker::DOTNET
        }
        _ if name.ends_with(b".uproject") => marker::UNREAL,
        _ => 0,
    }
}

/// Marker bits with derived kinds filled in.
fn project_kinds(bits: u16) -> u16 {
    let unity = marker::UNITY_ASSETS | marker::UNITY_SETTINGS;
    if bits & unity == unity {
        bits | marker::UNITY
    } else {
        bits
    }
}

// Folders with common names whose contents are skipped only when the folder next to
// them marks a project that produces them (e.g. `target` next to `Cargo.toml`).
const SKIP_IN_PROJECT: &[(&[u8], u16)] = &[
    (b"target", marker::RUST),
    (b"bin", marker::DOTNET),
    (b"obj", marker::DOTNET | marker::UNITY),
    (b"packages", marker::DOTNET),
    (
        b"build",
        marker::GRADLE | marker::CMAKE | marker::NODE | marker::FLUTTER,
    ),
    (b"dist", marker::NODE),
    (b"out", marker::NODE),
    (b"vendor", marker::COMPOSER | marker::GO),
    (b"pods", marker::PODS),
    (b"library", marker::UNITY),
    (b"temp", marker::UNITY),
    (b"logs", marker::UNITY),
    (b"intermediate", marker::UNREAL),
    (b"deriveddatacache", marker::UNREAL),
];

fn is_project_rule_name(name: &[u8]) -> bool {
    SKIP_IN_PROJECT.iter().any(|&(n, _)| n == name)
}

// Folders whose contents are skipped only inside system or app-data areas (entries
// with the Noisy location), where names like "cache" or "temp" are never user files.
const SKIP_IN_NOISY: &[&[u8]] = &[
    b"winsxs",
    b"servicing",
    b"softwaredistribution",
    b"installer",
    b"assembly",
    b"microsoft.net",
    b"driverstore",
    b"prefetch",
    b"logs",
    b"temp",
    b"tmp",
    b"wer",
    b"crashpad",
    b"crashdumps",
    b"cache",
    b"caches",
    b"code cache",
    b"gpucache",
    b"cachestorage",
    b"inetcache",
    b"webcache",
    b"shadercache",
    b"grshadercache",
    b"graphitedawncache",
    b"dawncache",
    b"dawngraphitecache",
    b"d3dscache",
    b"service worker",
    b"indexeddb",
    b"blob_storage",
    b"file system",
    b"package cache",
];

// Rule numbers: the lists above in order, then the fixed rules below.
const RULE_IN_NOISY: usize = SKIP_ANYWHERE.len();
const RULE_IN_PROJECT: usize = RULE_IN_NOISY + SKIP_IN_NOISY.len();
const RULE_ROOT_SYSTEM: usize = RULE_IN_PROJECT + SKIP_IN_PROJECT.len();
const RULE_CACHEDIR_TAG: usize = RULE_ROOT_SYSTEM + 1;
const RULE_PYVENV: usize = RULE_ROOT_SYSTEM + 2;
const RULE_CMAKE_CACHE: usize = RULE_ROOT_SYSTEM + 3;
const RULE_COUNT: usize = RULE_ROOT_SYSTEM + 4;
const _: () = assert!(RULE_COUNT <= u8::MAX as usize);

fn rule_name(rule: usize) -> String {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    match rule {
        r if r < RULE_IN_NOISY => text(SKIP_ANYWHERE[r]),
        r if r < RULE_IN_PROJECT => text(SKIP_IN_NOISY[r - RULE_IN_NOISY]),
        r if r < RULE_ROOT_SYSTEM => {
            format!(
                "{} in projects",
                text(SKIP_IN_PROJECT[r - RULE_IN_PROJECT].0)
            )
        }
        RULE_ROOT_SYSTEM => "$... and System Volume Information at drive roots".into(),
        RULE_CACHEDIR_TAG => "folders with CACHEDIR.TAG".into(),
        RULE_PYVENV => "Python virtual environments".into(),
        _ => "CMake build folders".into(),
    }
}

/// Which rule, if any, leaves out the contents of folder `entry`. `own` and `siblings`
/// are the marker bits of the folder's own contents and of its parent's contents.
fn skip_rule(index: &Index, entry: u32, own: u16, siblings: u16) -> Option<usize> {
    if !index.is_dir(entry) {
        return None;
    }
    let parent = index.parent(entry)?;
    let name = index.name_folded(entry);
    let at_root = index.parent(parent).is_none();
    if at_root && (name.starts_with(b"$") || name == b"system volume information") {
        return Some(RULE_ROOT_SYSTEM);
    }
    if let Some(i) = SKIP_ANYWHERE.iter().position(|&n| n == name) {
        return Some(i);
    }
    if index.location(parent) == Location::Noisy
        && let Some(i) = SKIP_IN_NOISY.iter().position(|&n| n == name)
    {
        return Some(RULE_IN_NOISY + i);
    }
    let kinds = project_kinds(siblings);
    if let Some(i) = SKIP_IN_PROJECT
        .iter()
        .position(|&(n, needs)| n == name && kinds & needs != 0)
    {
        return Some(RULE_IN_PROJECT + i);
    }
    if own & marker::CACHEDIR_TAG != 0 {
        return Some(RULE_CACHEDIR_TAG);
    }
    if own & marker::PYVENV != 0 {
        return Some(RULE_PYVENV);
    }
    if own & marker::CMAKE_CACHE != 0 {
        return Some(RULE_CMAKE_CACHE);
    }
    None
}

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
    fn live_changes_do_not_double_memory() {
        let mut b = IndexBuilder::new();
        b.begin_volume("C:", 5);
        for i in 0..200_000u64 {
            b.push(100 + i, 5, &format!("file number {i}.txt"), false, false);
        }
        b.end_volume();
        let mut index = b.finish();
        let before = index.memory_usage();
        for i in 0..10u64 {
            index.apply(0, upsert(1_000_000 + i, 5, &format!("new {i}.txt"), false));
        }
        index.apply(0, upsert(100, 5, "renamed.txt", false));
        index.end_batch();
        let after = index.memory_usage();
        let grown = |a: usize, b: usize| b as f64 / a as f64;
        assert!(grown(before.name_bytes, after.name_bytes) < 1.1);
        assert!(grown(before.entry_bytes, after.entry_bytes) < 1.1);
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
        assert!(
            index.volumes()[0]
                .entry_for_record(21)
                .is_none_or(|e| index.is_deleted(e))
        );
        index.compact();
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
        // Names are shared right away, also after compaction.
        index.apply(0, upsert(51, 5, "calc.exe", false));
        index.end_batch();
        assert_eq!(index.names().len(), names_before - 2);
    }

    #[test]
    fn compaction_keeps_case_variants_apart() {
        let mut index = sample();
        index.apply(0, upsert(50, 5, "Report.docx", false));
        index.apply(0, upsert(51, 5, "report.docx", false));
        index.end_batch();
        index.compact();
        find(&index, "C:\\Report.docx");
        find(&index, "C:\\report.docx");
    }

    #[test]
    fn records_stay_findable_through_changes_and_compaction() {
        let mut index = sample();
        // New records below, between and above the scanned ones.
        index.apply(0, upsert(7, 5, "low.txt", false));
        index.apply(0, upsert(25, 20, "mid.txt", false));
        index.apply(0, upsert(900, 31, "high.txt", false));
        // Record 13 (report.docx) is deleted and its number reused for a new file.
        index.apply(0, Change::Delete { record: 13 });
        index.apply(0, upsert(13, 12, "reused.txt", false));
        index.end_batch();
        let check = |index: &Index| {
            for (record, path) in [
                (7, "C:\\low.txt"),
                (25, "C:\\Windows\\mid.txt"),
                (900, "C:\\code\\high.txt"),
                (13, "C:\\Users\\bob\\Documents\\reused.txt"),
                (21, "C:\\Windows\\notepad.exe"),
                (5, "C:\\"),
            ] {
                let entry = index.volumes()[0].entry_for_record(record).unwrap();
                assert_eq!(index.full_path(entry), path, "record {record}");
            }
        };
        check(&index);
        index.compact();
        check(&index);
        assert!(index.volumes()[0].records.recent.is_empty());
        assert!(try_find(&index, "C:\\Users\\bob\\Documents\\report.docx").is_none());
        // Changes keep working on the compacted index.
        index.apply(0, upsert(900, 5, "high.txt", false));
        index.end_batch();
        find(&index, "C:\\high.txt");
    }

    #[test]
    fn new_files_with_unknown_folders_are_ignored() {
        let mut index = sample();
        assert_eq!(
            index.apply(0, upsert(60, 12345, "lost.txt", false)),
            Applied::Unchanged
        );
    }

    #[test]
    fn moving_into_a_skipped_folder_removes_the_whole_subtree() {
        let mut index = clutter_sample();
        index.skip_clutter();
        // Move C:\Users\bob\Documents (with cache\notes.txt inside) into node_modules.
        let applied = index.apply(0, upsert(50, 22, "Documents", true));
        index.end_batch();
        assert!(matches!(applied, Applied::Deleted(_)));
        assert!(try_find(&index, "C:\\Users\\bob\\Documents").is_none());
        assert!(try_find(&index, "C:\\Users\\bob\\Documents\\cache\\notes.txt").is_none());
        let deleted = index.deleted_len();
        assert_eq!(deleted, 3);
        index.compact();
        assert_eq!(index.deleted_len(), 0);
        find(&index, "C:\\code\\app\\node_modules");
    }

    #[test]
    fn compaction_is_needed_only_after_enough_changes() {
        let mut b = IndexBuilder::new();
        b.begin_volume("C:", 5);
        for i in 0..10_000u64 {
            b.push(100 + i, 5, &format!("f{i}"), false, false);
        }
        b.end_volume();
        let mut index = b.finish();
        for i in 0..100u64 {
            index.apply(0, Change::Delete { record: 100 + i });
        }
        index.end_batch();
        assert!(!index.needs_compaction());
        for i in 100..300u64 {
            index.apply(0, Change::Delete { record: 100 + i });
        }
        index.end_batch();
        assert!(index.needs_compaction());
        index.compact();
        assert!(!index.needs_compaction());
    }

    /// C:\Users\bob\AppData\Local\Temp\x.tmp, C:\code\app\node_modules\lib\a.js,
    /// C:\Windows\WinSxS\big.dll, C:\Windows\notepad.exe, C:\$Recycle.Bin\$R1.txt,
    /// C:\Users\bob\Documents\cache\notes.txt
    fn clutter_sample() -> Index {
        let mut b = IndexBuilder::new();
        b.begin_volume("C:", 5);
        b.push(10, 5, "Users", true, false);
        b.push(11, 10, "bob", true, false);
        b.push(12, 11, "AppData", true, true);
        b.push(13, 12, "Local", true, false);
        b.push(14, 13, "Temp", true, false);
        b.push(15, 14, "x.tmp", false, false);
        b.push(20, 5, "code", true, false);
        b.push(21, 20, "app", true, false);
        b.push(22, 21, "node_modules", true, false);
        b.push(23, 22, "lib", true, false);
        b.push(24, 23, "a.js", false, false);
        b.push(30, 5, "Windows", true, false);
        b.push(31, 30, "WinSxS", true, false);
        b.push(32, 31, "big.dll", false, false);
        b.push(33, 30, "notepad.exe", false, false);
        b.push(40, 5, "$Recycle.Bin", true, true);
        b.push(41, 40, "$R1.txt", false, false);
        b.push(50, 11, "Documents", true, false);
        b.push(51, 50, "cache", true, false);
        b.push(52, 51, "notes.txt", false, false);
        b.end_volume();
        b.finish()
    }

    #[test]
    fn skips_clutter_contents_but_keeps_the_folders() {
        let mut index = clutter_sample();
        let report = index.skip_clutter();
        assert_eq!(report.removed, 5);
        let count = |rule: &str| {
            report
                .by_rule
                .iter()
                .find(|(name, _)| name == rule)
                .map_or(0, |&(_, n)| n)
        };
        assert_eq!(count("node_modules"), 2);
        assert_eq!(count("temp"), 1);
        assert_eq!(count("winsxs"), 1);
        assert_eq!(index.skip_rules(), SKIP_RULES_VERSION);
        for gone in [
            "C:\\Users\\bob\\AppData\\Local\\Temp\\x.tmp",
            "C:\\code\\app\\node_modules\\lib",
            "C:\\code\\app\\node_modules\\lib\\a.js",
            "C:\\Windows\\WinSxS\\big.dll",
            "C:\\$Recycle.Bin\\$R1.txt",
        ] {
            assert!(try_find(&index, gone).is_none(), "{gone} should be skipped");
        }
        for kept in [
            "C:\\Users\\bob\\AppData\\Local\\Temp",
            "C:\\code\\app\\node_modules",
            "C:\\Windows\\WinSxS",
            "C:\\Windows\\notepad.exe",
            // "cache" is only clutter inside system or app-data folders.
            "C:\\Users\\bob\\Documents\\cache\\notes.txt",
        ] {
            find(&index, kept);
        }
    }

    #[test]
    fn new_files_inside_skipped_folders_stay_out() {
        let mut index = clutter_sample();
        index.skip_clutter();
        // Directly inside a skipped folder, and inside a subfolder that was skipped.
        let a = index.apply(0, upsert(60, 22, "b.js", false));
        let b = index.apply(0, upsert(61, 23, "c.js", false));
        // A new folder that gets skipped, then a file inside it.
        let c = index.apply(0, upsert(62, 21, ".git", true));
        let d = index.apply(0, upsert(63, 62, "HEAD", false));
        index.end_batch();
        assert_eq!(a, Applied::Unchanged);
        assert_eq!(b, Applied::Unchanged);
        assert!(matches!(c, Applied::Created(_)));
        assert_eq!(d, Applied::Unchanged);
        find(&index, "C:\\code\\app\\.git");
        assert!(try_find(&index, "C:\\code\\app\\.git\\HEAD").is_none());
        // Deleting a skipped file is harmless, and a normal new file is still indexed.
        assert_eq!(
            index.apply(0, Change::Delete { record: 24 }),
            Applied::Unchanged
        );
        index.apply(0, upsert(64, 21, "main.js", false));
        index.end_batch();
        find(&index, "C:\\code\\app\\main.js");
    }

    /// C:\rust\{Cargo.toml, target\debug\app.exe, src\main.rs}
    /// C:\notes\target\plan.txt          (no Cargo.toml: kept)
    /// C:\web\{package.json, dist\app.js, build\x.js}
    /// C:\Music\Library\song.mp3         (no Unity project: kept)
    /// C:\game\{Assets\, ProjectSettings\, Library\a.asset}
    /// C:\env\{pyvenv.cfg, Lib\x.py}
    /// C:\data\cache\{CACHEDIR.TAG, blob}
    /// C:\cs\{App.csproj, bin\App.dll, obj\x.json}
    fn project_sample() -> Index {
        let mut b = IndexBuilder::new();
        b.begin_volume("C:", 5);
        b.push(10, 5, "rust", true, false);
        b.push(11, 10, "Cargo.toml", false, false);
        b.push(12, 10, "target", true, false);
        b.push(13, 12, "debug", true, false);
        b.push(14, 13, "app.exe", false, false);
        b.push(15, 10, "src", true, false);
        b.push(16, 15, "main.rs", false, false);
        b.push(20, 5, "notes", true, false);
        b.push(21, 20, "target", true, false);
        b.push(22, 21, "plan.txt", false, false);
        b.push(30, 5, "web", true, false);
        b.push(31, 30, "package.json", false, false);
        b.push(32, 30, "dist", true, false);
        b.push(33, 32, "app.js", false, false);
        b.push(34, 30, "build", true, false);
        b.push(35, 34, "x.js", false, false);
        b.push(40, 5, "Music", true, false);
        b.push(41, 40, "Library", true, false);
        b.push(42, 41, "song.mp3", false, false);
        b.push(50, 5, "game", true, false);
        b.push(51, 50, "Assets", true, false);
        b.push(52, 50, "ProjectSettings", true, false);
        b.push(53, 50, "Library", true, false);
        b.push(54, 53, "a.asset", false, false);
        b.push(60, 5, "env", true, false);
        b.push(61, 60, "pyvenv.cfg", false, false);
        b.push(62, 60, "Lib", true, false);
        b.push(63, 62, "x.py", false, false);
        b.push(70, 5, "data", true, false);
        b.push(71, 70, "cache", true, false);
        b.push(72, 71, "CACHEDIR.TAG", false, false);
        b.push(73, 71, "blob", false, false);
        b.push(80, 5, "cs", true, false);
        b.push(81, 80, "App.csproj", false, false);
        b.push(82, 80, "bin", true, false);
        b.push(83, 82, "App.dll", false, false);
        b.push(84, 80, "obj", true, false);
        b.push(85, 84, "x.json", false, false);
        b.end_volume();
        b.finish()
    }

    #[test]
    fn project_folders_are_skipped_only_next_to_their_marker() {
        let mut index = project_sample();
        let report = index.skip_clutter();
        for gone in [
            "C:\\rust\\target\\debug",
            "C:\\web\\dist\\app.js",
            "C:\\web\\build\\x.js",
            "C:\\game\\Library\\a.asset",
            "C:\\env\\pyvenv.cfg",
            "C:\\env\\Lib\\x.py",
            "C:\\data\\cache\\blob",
            "C:\\cs\\bin\\App.dll",
            "C:\\cs\\obj\\x.json",
        ] {
            assert!(try_find(&index, gone).is_none(), "{gone} should be skipped");
        }
        for kept in [
            "C:\\rust\\target",
            "C:\\rust\\src\\main.rs",
            "C:\\notes\\target\\plan.txt",
            "C:\\Music\\Library\\song.mp3",
            "C:\\env",
            "C:\\data\\cache",
            "C:\\game\\Assets",
        ] {
            find(&index, kept);
        }
        let names: Vec<&str> = report.by_rule.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"target in projects"), "{names:?}");
        assert!(names.contains(&"Python virtual environments"), "{names:?}");
        assert!(names.contains(&"folders with CACHEDIR.TAG"), "{names:?}");
    }

    #[test]
    fn late_markers_skip_existing_folders() {
        let mut index = project_sample();
        index.skip_clutter();
        // A cloned project: `target` with contents exists before `Cargo.toml` arrives.
        index.apply(0, upsert(100, 5, "clone", true));
        index.apply(0, upsert(101, 100, "target", true));
        index.apply(0, upsert(102, 101, "out.rlib", false));
        index.end_batch();
        find(&index, "C:\\clone\\target\\out.rlib");
        index.apply(0, upsert(103, 100, "Cargo.toml", false));
        index.end_batch();
        assert!(try_find(&index, "C:\\clone\\target\\out.rlib").is_none());
        find(&index, "C:\\clone\\target");
        // Later files inside it stay out.
        assert_eq!(
            index.apply(0, upsert(104, 101, "more.rlib", false)),
            Applied::Unchanged
        );
        // A folder labels itself as a cache after it already has files.
        index.apply(0, upsert(110, 5, "tool-cache", true));
        index.apply(0, upsert(111, 110, "entry", false));
        index.end_batch();
        find(&index, "C:\\tool-cache\\entry");
        index.apply(0, upsert(112, 110, "CACHEDIR.TAG", false));
        index.end_batch();
        assert!(try_find(&index, "C:\\tool-cache\\entry").is_none());
        find(&index, "C:\\tool-cache");
    }

    #[test]
    fn new_project_folders_and_renames_are_skipped() {
        let mut index = project_sample();
        index.skip_clutter();
        // A new `out` folder next to package.json, created together with a file in it.
        index.apply(0, upsert(120, 30, "out", true));
        index.apply(0, upsert(121, 120, "page.html", false));
        index.end_batch();
        find(&index, "C:\\web\\out");
        assert!(try_find(&index, "C:\\web\\out\\page.html").is_none());
        // A normal folder renamed to node_modules loses its contents.
        index.apply(0, upsert(15, 10, "node_modules", true));
        index.end_batch();
        find(&index, "C:\\rust\\node_modules");
        assert!(try_find(&index, "C:\\rust\\node_modules\\main.rs").is_none());
        // Everything is still consistent after compaction.
        index.compact();
        find(&index, "C:\\rust\\node_modules");
        find(&index, "C:\\notes\\target\\plan.txt");
    }

    #[test]
    fn skipped_state_survives_save_and_load() {
        let mut index = clutter_sample();
        index.skip_clutter();
        let path = std::env::temp_dir().join(format!("bs-skip-{}.bin", std::process::id()));
        index.save(&path).unwrap();
        let mut loaded = Index::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(loaded.skip_rules(), SKIP_RULES_VERSION);
        assert_eq!(loaded.live_len(), index.live_len());
        assert_eq!(
            loaded.apply(0, upsert(60, 23, "d.js", false)),
            Applied::Unchanged
        );
    }

    #[test]
    fn users_epoch_changes_only_when_profiles_change() {
        // sample(): C:\Users\bob\Documents\report.docx, C:\Windows\notepad.exe, record
        // numbers 10 (Users), 11 (bob), 12 (Documents), 13 (report.docx).
        // sample(): C:\Users (record 10) \ bob (11) \ Documents (12) \ report.docx (13),
        // and C:\Windows (20), C:\orphan.txt (40).
        let mut index = sample();
        let start = index.users_epoch();

        // A new file inside a profile folder inherits its owner; no rebuild needed.
        index.apply(0, upsert(300, 11, "notes.txt", false));
        index.end_batch();
        assert_eq!(index.users_epoch(), start);

        // Renaming a file inside a profile folder also changes nothing.
        index.apply(0, upsert(300, 11, "notes2.txt", false));
        index.end_batch();
        assert_eq!(index.users_epoch(), start);

        // A new profile folder changes who owns what until the map is rebuilt.
        index.apply(0, upsert(301, 10, "anna", true));
        index.end_batch();
        let after_new_profile = index.users_epoch();
        assert_ne!(after_new_profile, start);

        // Moving a folder from bob's profile into anna's changes its owner.
        index.apply(0, upsert(12, 301, "Documents", true));
        index.end_batch();
        assert_ne!(index.users_epoch(), after_new_profile);
        assert_eq!(
            index.parent(index.volumes()[0].entry_for_record(12).unwrap()),
            index.volumes()[0].entry_for_record(301)
        );

        // Moving it out of any profile folder gives up its owner too.
        let moved = index.volumes()[0].entry_for_record(12).unwrap();
        let windows = index.volumes()[0].entry_for_record(20).unwrap();
        index.apply(0, upsert(12, 20, "Documents", true));
        index.end_batch();
        assert_eq!(index.parent(moved), Some(windows));

        // Compaction renumbers entries, so any map built from them is void.
        let before_compaction = index.users_epoch();
        index.compact();
        assert_ne!(index.users_epoch(), before_compaction);
    }
}
