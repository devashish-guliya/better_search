//! Ranked, case-insensitive file name search over a [`bs_index::Index`].
//!
//! The search runs in two parallel passes:
//! 1. Score every *unique* name once (repeated names like `index.js` are checked once).
//!    Names are stored lowercased back to back, so for ASCII queries this is one SIMD
//!    substring scan over the whole buffer; only names containing a hit get scored.
//! 2. Walk all entries, look up the score of their name, adjust it by location, and keep
//!    only the best `limit` hits per thread. The full match list is never materialized,
//!    so memory stays small even when millions of entries match.
//!
//! [`Session`] adds type-ahead narrowing: when a query only adds characters to the
//! previous one, only the names that matched before are checked again.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bs_index::{Index, Location, NameTable, flags};
use memchr::memmem::{self, Finder};
use rayon::prelude::*;

/// A parsed query: whitespace-separated terms that must all appear in the name, or in
/// the names of the folders above it (see [`Query::partial`]), plus an optional
/// `ext:pdf,docx` filter.
pub struct Query {
    terms: Vec<Finder<'static>>,
    /// All terms are ASCII, so names can be compared in their stored lowercased form.
    ascii: bool,
    /// Term used to scan the name buffer: the longest, as it produces the fewest hits.
    scan_term: usize,
    /// Leave entries in system, app-data and program folders out of the hits and the
    /// match count, and count them in `hidden_matches` instead.
    hide_system: bool,
    /// Lowercased extensions without the dot; empty means any.
    extensions: Vec<Vec<u8>>,
    /// Several terms, so a term the name lacks may be found in a parent folder's name
    /// instead (`acme contract` finds `Clients\Acme\Contract.pdf`). Term masks are a
    /// `u8`, which caps this at [`MAX_PARTIAL_TERMS`].
    partial: bool,
}

const MAX_PARTIAL_TERMS: usize = 8;

impl Query {
    /// Whether to hide entries in system, app-data and program folders (the `Noisy` and
    /// `AppFiles` locations). Off by default.
    pub fn hiding_system(mut self, hide: bool) -> Self {
        self.hide_system = hide;
        self
    }

    pub fn parse(input: &str) -> Option<Self> {
        let mut extensions: Vec<Vec<u8>> = Vec::new();
        let mut words: Vec<String> = Vec::new();
        for word in input.split_whitespace() {
            let lower = word.to_lowercase();
            match lower.strip_prefix("ext:") {
                Some(list) => extensions.extend(
                    list.split(',')
                        .map(|e| e.trim_start_matches('.'))
                        .filter(|e| !e.is_empty())
                        .map(|e| e.as_bytes().to_vec()),
                ),
                None => words.push(lower),
            }
        }
        if words.is_empty() {
            // An extension filter alone lists every file of that type.
            match extensions.as_slice() {
                [] => return None,
                [only] => words.push(format!(".{}", String::from_utf8_lossy(only))),
                _ => words.push(".".into()),
            }
        }
        let terms: Vec<Finder<'static>> = words
            .iter()
            .map(|t| Finder::new(t.as_bytes()).into_owned())
            .collect();
        let ascii = terms.iter().all(|t| t.needle().is_ascii());
        let scan_term = (0..terms.len())
            .max_by_key(|&i| terms[i].needle().len())
            .unwrap_or(0);
        let partial = (2..=MAX_PARTIAL_TERMS).contains(&terms.len());
        Some(Self {
            terms,
            ascii,
            scan_term,
            hide_system: false,
            extensions,
            partial,
        })
    }

    /// Mask with one bit per term.
    fn full_mask(&self) -> u8 {
        if self.partial {
            ((1u16 << self.terms.len()) - 1) as u8
        } else {
            1
        }
    }

    /// True when every name `self` can match was among the names the `previous` query
    /// matched (see [`Session`]).
    fn narrows(&self, previous: &LastSearch) -> bool {
        if previous.extensions != self.extensions {
            return false;
        }
        let old_terms = &previous.terms;
        if self.partial {
            // A name may match just one of our terms, so each of our terms must imply
            // one of the old ones, and the old query must have kept partial matches
            // too (or had a single term).
            old_terms.len() <= MAX_PARTIAL_TERMS
                && self.terms.iter().all(|new| {
                    old_terms
                        .iter()
                        .any(|old| memmem::find(new.needle(), old).is_some())
                })
        } else {
            // A name matches all our terms, so it contains every old term.
            old_terms.iter().all(|old| {
                self.terms
                    .iter()
                    .any(|new| memmem::find(new.needle(), old).is_some())
            })
        }
    }

    fn term_bytes(&self) -> Vec<Vec<u8>> {
        self.terms.iter().map(|t| t.needle().to_vec()).collect()
    }

    /// Whether the lowercased name passes the `ext:` filter.
    fn extension_ok(&self, lower: &[u8]) -> bool {
        self.extensions.is_empty()
            || memchr::memrchr(b'.', lower)
                .is_some_and(|dot| self.extensions.iter().any(|e| e[..] == lower[dot + 1..]))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub entry: u32,
    pub score: i32,
    /// Folder depth, the first tie-breaker: of equal matches, the less buried one wins.
    pub depth: u16,
}

/// Per-name results of pass 1.
struct NameScores {
    /// 0 when the name matches no term.
    scores: Vec<u8>,
    /// Which terms each name matched, for queries with [`Query::partial`]; empty
    /// otherwise, where a non-zero score means every term matched.
    masks: Vec<u8>,
}

pub struct SearchResult {
    /// Best hits, highest score first.
    pub hits: Vec<Hit>,
    /// Number of entries that matched, including those not in `hits`.
    pub total_matches: usize,
    /// Matches left out because the query hides system folders (see
    /// [`Query::hiding_system`]). They are not part of `total_matches`.
    pub hidden_matches: usize,
}

impl SearchResult {
    fn empty() -> Self {
        Self {
            hits: Vec::new(),
            total_matches: 0,
            hidden_matches: 0,
        }
    }
}

/// Decides per entry whether it may appear in results; `false` hides it from both the
/// hits and the match count.
pub type Filter<'a> = &'a (dyn Fn(u32) -> bool + Sync);

/// One-off search. Use [`Session`] for searches that follow the user's typing.
pub fn search(index: &Index, query: &Query, limit: usize) -> SearchResult {
    search_filtered(index, query, limit, None)
}

pub fn search_filtered(
    index: &Index,
    query: &Query,
    limit: usize,
    filter: Option<Filter<'_>>,
) -> SearchResult {
    let scores = score_names(index.names(), query, None);
    let result = if collect_matches(&scores.scores, None, false).0 == 0 {
        SearchResult::empty()
    } else {
        rank(index, &scores, query, limit, filter)
    };
    add_acronyms(index, query, limit, filter, result)
}

/// Name score of an acronym match (`vsc` for `Visual Studio Code`): above a match in
/// the middle of a word, below a prefix match.
const ACRONYM: i32 = 60;

/// Acronym queries are 2 to 6 letters.
const ACRONYM_LENGTHS: std::ops::RangeInclusive<usize> = 2..=6;

/// Acronyms are a fallback for searches with few ordinary matches. With more, the
/// user is looking at plenty already, and the extra scan would cost every keystroke.
const ACRONYM_BELOW: usize = 5000;

/// Adds names whose word initials spell the query. Only a single ASCII word of letters
/// can be an acronym. Ordinary matches keep their score, so an acronym only wins
/// where it also gets a boost from its location or type (a Start Menu shortcut).
fn add_acronyms(
    index: &Index,
    query: &Query,
    limit: usize,
    filter: Option<Filter<'_>>,
    found: SearchResult,
) -> SearchResult {
    let [term] = query.terms.as_slice() else {
        return found;
    };
    if found.total_matches >= ACRONYM_BELOW {
        return found;
    }
    let word = term.needle();
    if !query.ascii
        || !ACRONYM_LENGTHS.contains(&word.len())
        || !word.iter().all(u8::is_ascii_alphabetic)
    {
        return found;
    }
    let names = index.names();
    let scores: Vec<u8> = (0..names.len() as u32)
        .into_par_iter()
        .with_min_len(1 << 14)
        .map(|id| acronym_score(names, id, word, query))
        .collect();
    if !scores.iter().any(|&s| s != 0) {
        return found;
    }
    let scores = NameScores {
        scores,
        masks: Vec::new(),
    };
    let extra = rank(index, &scores, query, limit, filter);
    if extra.total_matches == 0 && extra.hidden_matches == 0 {
        return found;
    }
    let mut hits = found.hits;
    let present: std::collections::HashSet<u32> = hits.iter().map(|h| h.entry).collect();
    // An entry that matched both ways is counted once. Overlaps that fall outside the
    // returned hits cannot be seen, so the total may be slightly high.
    let mut duplicates = 0;
    for hit in extra.hits {
        if present.contains(&hit.entry) {
            duplicates += 1;
        } else {
            hits.push(hit);
        }
    }
    hits.sort_unstable_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.depth.cmp(&b.depth))
            .then(a.entry.cmp(&b.entry))
    });
    hits.truncate(limit);
    SearchResult {
        hits,
        total_matches: found.total_matches + extra.total_matches - duplicates,
        hidden_matches: found.hidden_matches + extra.hidden_matches,
    }
}

/// [`ACRONYM`] (adjusted for the file type) when the word initials of the name, without
/// its extension, are exactly `word`; 0 otherwise.
fn acronym_score(names: &NameTable, id: u32, word: &[u8], query: &Query) -> u8 {
    let folded = names.folded(id);
    if folded.first() != word.first() || !query.extension_ok(folded) {
        return 0;
    }
    let start = names.range(id).start;
    let extension = memchr::memrchr(b'.', folded)
        .filter(|&dot| dot > 0 && folded.len() - dot <= 6)
        .map(|dot| &folded[dot + 1..]);
    let end = extension.map_or(folded.len(), |ext| folded.len() - ext.len() - 1);
    let mut next = 0;
    for i in 0..end {
        let cur = folded[i];
        if !cur.is_ascii_alphanumeric() {
            continue;
        }
        let word_start = i == 0
            || (!folded[i - 1].is_ascii_alphanumeric() && folded[i - 1].is_ascii())
            || (folded[i - 1].is_ascii_lowercase()
                && !names.is_upper(start + i - 1)
                && cur.is_ascii_lowercase()
                && names.is_upper(start + i));
        if !word_start {
            continue;
        }
        if next == word.len() || word[next] != cur {
            return 0;
        }
        next += 1;
    }
    if next != word.len() {
        return 0;
    }
    let mut score = ACRONYM;
    match extension {
        Some(ext) if LAUNCHABLE.contains(&ext) => score += 10,
        Some(ext) if DEMOTED.contains(&ext) => score -= 10,
        _ => {}
    }
    score as u8
}

/// Counts the names whose score is non-zero, and (when `keep` is set and there are few
/// enough of them to be worth remembering) lists their ids. Counting is much cheaper
/// than building the list, so a broad query never materializes one.
fn collect_matches(
    scores: &[u8],
    candidates: Option<&[u32]>,
    keep: bool,
) -> (usize, Option<Vec<u32>>) {
    let hit = |id: u32| scores[id as usize] != 0;
    match candidates {
        Some(previous) => {
            let count = previous.iter().filter(|&&id| hit(id)).count();
            let list = (keep && count <= MAX_NARROW_NAMES)
                .then(|| previous.iter().copied().filter(|&id| hit(id)).collect());
            (count, list)
        }
        None => {
            let count = scores.par_iter().filter(|&&s| s != 0).count();
            let list = (keep && count <= MAX_NARROW_NAMES).then(|| {
                scores
                    .par_iter()
                    .enumerate()
                    .filter(|&(_, &s)| s != 0)
                    .map(|(id, _)| id as u32)
                    .collect()
            });
            (count, list)
        }
    }
}

/// Remembers which names matched the previous query so the next keystroke only has to
/// re-check those. Automatically starts over when the index changes, and falls back to
/// a full scan when the previous query matched a large share of all names.
#[derive(Default)]
pub struct Session {
    last: Option<LastSearch>,
}

struct LastSearch {
    generation: u64,
    terms: Vec<Vec<u8>>,
    extensions: Vec<Vec<u8>>,
    /// `None` when the last query matched too many names for narrowing to pay off.
    matched_names: Option<Vec<u32>>,
}

/// Longest match list that is remembered for narrowing. Checking remembered names
/// costs about 80 ns each (measured on the development machine at 12k names: 3.7 ms
/// narrowed against 2.7 ms for a fresh search), so only short lists are worth keeping.
const MAX_NARROW_NAMES: usize = 8192;

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn search(&mut self, index: &Index, query: &Query, limit: usize) -> SearchResult {
        self.search_filtered(index, query, limit, None)
    }

    /// The remembered names do not depend on `filter`, so it may differ between calls.
    pub fn search_filtered(
        &mut self,
        index: &Index,
        query: &Query,
        limit: usize,
        filter: Option<Filter<'_>>,
    ) -> SearchResult {
        let candidates = self
            .last
            .as_ref()
            .filter(|last| last.generation == index.generation() && query.narrows(last))
            .and_then(|last| last.matched_names.as_deref());
        let scores = score_names(index.names(), query, candidates);
        let (matched, stored) = collect_matches(&scores.scores, candidates, true);
        let result = if matched == 0 {
            SearchResult::empty()
        } else {
            rank(index, &scores, query, limit, filter)
        };
        self.last = Some(LastSearch {
            generation: index.generation(),
            terms: query.term_bytes(),
            extensions: query.extensions.clone(),
            matched_names: stored,
        });
        add_acronyms(index, query, limit, filter, result)
    }
}

const EXACT: i32 = 100;
/// A launchable file named exactly after the query (`factory.exe`). This is the app
/// the user meant, so it outranks folders and installers that merely share a prefix.
const APP_STEM: i32 = 120;
const EXACT_STEM: i32 = 90;
const PREFIX: i32 = 70;
/// Close to a prefix: document names often lead with a date or "Copy of".
const WORD_START: i32 = 56;
const SUBSTRING: i32 = 30;
/// What a term found only in a parent folder's name counts for: less than any match
/// in the name itself.
const FOLDER_TERM: i32 = 20;
/// Name score ceiling of Start Menu filler such as `Readme.lnk` or `Acme Website.url`,
/// so the Start Menu boost does not lift them above the user's own files.
const GENERIC_SHORTCUT: i32 = 20;

/// Extensions the user launches: programs, shortcuts and system tools.
const LAUNCHABLE: &[&[u8]] = &[
    b"exe",
    b"com",
    b"bat",
    b"cmd",
    b"msi",
    b"lnk",
    b"url",
    b"appref-ms",
    b"msc",
    b"cpl",
];

/// Documents a consumer user opens: office files, PDFs, notes, e-mail, e-books and
/// archives.
const DOCUMENTS: &[&[u8]] = &[
    b"pdf", b"doc", b"docx", b"docm", b"odt", b"rtf", b"txt", b"md", b"xls", b"xlsx", b"xlsm",
    b"ods", b"csv", b"ppt", b"pptx", b"odp", b"one", b"vsdx", b"pub", b"msg", b"eml", b"epub",
    b"mobi", b"zip", b"rar", b"7z",
];

/// Photos, video and audio: just under documents.
const MEDIA: &[&[u8]] = &[
    b"jpg", b"jpeg", b"png", b"gif", b"bmp", b"webp", b"heic", b"avif", b"tif", b"tiff", b"svg",
    b"psd", b"dng", b"cr2", b"cr3", b"nef", b"arw", b"mp4", b"m4v", b"mkv", b"mov", b"avi",
    b"webm", b"wmv", b"mp3", b"wav", b"flac", b"m4a", b"aac", b"ogg", b"wma", b"opus",
];

/// Shortcut names (without extension) that vendors put next to the app's own
/// shortcut, alone or after the app name (`Acme Help`).
const GENERIC_SHORTCUTS: &[&[u8]] = &[
    b"readme",
    b"read me",
    b"help",
    b"online help",
    b"manual",
    b"user manual",
    b"user guide",
    b"documentation",
    b"license",
    b"license agreement",
    b"eula",
    b"website",
    b"web site",
    b"home page",
    b"homepage",
    b"on the web",
    b"release notes",
    b"changelog",
    b"what's new",
    b"faq",
    b"support",
];

fn is_generic_shortcut(stem: &[u8]) -> bool {
    GENERIC_SHORTCUTS.iter().any(|g| {
        stem == *g
            || (stem.len() > g.len() + 1
                && stem.ends_with(g)
                && stem[stem.len() - g.len() - 1] == b' ')
    })
}

/// Source and configuration files a developer wrote: shown, but after everything a
/// regular user is likelier to want.
const CODE: &[&[u8]] = &[
    b"rs", b"py", b"js", b"jsx", b"ts", b"tsx", b"json", b"h", b"hpp", b"c", b"cc", b"cpp", b"cs",
    b"java", b"go", b"rb", b"php", b"kt", b"swift", b"toml", b"yaml", b"yml", b"xml", b"ini",
    b"cfg", b"conf", b"sh", b"ps1", b"css", b"scss", b"html", b"htm", b"sql", b"gradle",
];

const DOCUMENT_BONUS: i32 = 14;
const MEDIA_BONUS: i32 = 9;
const CODE_PENALTY: i32 = 8;
const APP_BONUS: i32 = 15;
/// Added to names that equal the query (with or without an extension) so they outrank
/// every kind and location preference.
const EXACT_BONUS: i32 = 40;

/// How the file type shifts a score: apps, then documents, then media; code last.
fn kind_bonus(extension: &[u8]) -> i32 {
    if DOCUMENTS.contains(&extension) {
        DOCUMENT_BONUS
    } else if MEDIA.contains(&extension) {
        MEDIA_BONUS
    } else if CODE.contains(&extension) {
        -CODE_PENALTY
    } else {
        0
    }
}

/// Extensions that are almost never the file someone searched for by name: binaries,
/// runtime files, logs and certificates.
const DEMOTED: &[&[u8]] = &[
    b"class",
    b"o",
    b"map",
    b"lock",
    b"dll",
    b"mui",
    b"tmp",
    b"log",
    b"etl",
    b"cat",
    b"manifest",
    b"pf",
    b"pyc",
    b"pem",
    b"pid",
    b"crt",
    b"key",
    b"pdb",
    b"lib",
    b"obj",
    b"ilk",
];

/// Pass 2: turns per-name scores into the best entries.
fn rank(
    index: &Index,
    names: &NameScores,
    query: &Query,
    limit: usize,
    filter: Option<Filter<'_>>,
) -> SearchResult {
    const CHUNK: usize = 1 << 14;
    let name_ids = index.name_ids();
    let entry_flags = index.flags();
    let full = query.full_mask();
    let masks = &names.masks;
    let top = name_ids
        .par_chunks(CHUNK)
        .zip(entry_flags.par_chunks(CHUNK))
        .enumerate()
        .fold(
            || (TopK::new(limit), FolderCache::new()),
            |(mut top, mut folders), (chunk, (ids, fl))| {
                let base = (chunk * CHUNK) as u32;
                for (k, (&name_id, &f)) in ids.iter().zip(fl).enumerate() {
                    let name_score = names.scores[name_id as usize];
                    if name_score == 0 || f & flags::DELETED != 0 {
                        continue;
                    }
                    let entry = base + k as u32;
                    // Volume roots ("C:") are not useful results.
                    let Some(parent) = index.parent(entry) else {
                        continue;
                    };
                    // Terms missing from the name must appear in the folders above it.
                    if !masks.is_empty() {
                        let mask = masks[name_id as usize];
                        if mask != full && mask | folders.get(index, masks, parent).mask != full {
                            continue;
                        }
                    }
                    if filter.is_none_or(|f| f(entry)) {
                        if query.hide_system && is_system(f) {
                            top.hidden += 1;
                        } else {
                            top.push(final_score(name_score, f), entry, || {
                                folders.get(index, masks, parent).depth.saturating_add(1)
                            });
                        }
                    }
                }
                (top, folders)
            },
        )
        .map(|(top, _)| top)
        .reduce(|| TopK::new(limit), TopK::merge);
    dedupe_shortcuts(index, top.into_result())
}

#[derive(Clone, Copy)]
struct FolderInfo {
    /// Terms matched by the folder or any folder above it, the volume root excepted
    /// (or "c" would match every folder on drive C:).
    mask: u8,
    /// Number of folders above it.
    depth: u16,
}

/// Remembers [`FolderInfo`] of recently seen folders. Entries of one folder are mostly
/// stored next to each other, so most lookups skip the walk up to the root.
struct FolderCache {
    slots: Vec<(u32, FolderInfo)>,
}

impl FolderCache {
    const SLOTS: usize = 4096;

    fn new() -> Self {
        Self {
            slots: vec![(NO_FOLDER, FolderInfo { mask: 0, depth: 0 }); Self::SLOTS],
        }
    }

    fn get(&mut self, index: &Index, masks: &[u8], folder: u32) -> FolderInfo {
        let slot = (folder.wrapping_mul(0x9E37_79B1) >> 20) as usize % Self::SLOTS;
        if self.slots[slot].0 == folder {
            return self.slots[slot].1;
        }
        let name_ids = index.name_ids();
        let mut info = FolderInfo { mask: 0, depth: 0 };
        let mut current = folder;
        while let Some(up) = index.parent(current)
            && info.depth < MAX_FOLDER_DEPTH
        {
            if !masks.is_empty() {
                info.mask |= masks[name_ids[current as usize] as usize];
            }
            info.depth += 1;
            current = up;
        }
        self.slots[slot] = (folder, info);
        info
    }
}

const NO_FOLDER: u32 = u32::MAX;
/// Same bound the index uses for parent chains.
const MAX_FOLDER_DEPTH: u16 = 512;

/// The same app often has a Start Menu shortcut for all users and another for the
/// current user. Only the better-ranked one is listed.
fn dedupe_shortcuts(index: &Index, mut result: SearchResult) -> SearchResult {
    let mut seen = std::collections::HashSet::new();
    let before = result.hits.len();
    result.hits.retain(|hit| {
        let f = index.flags()[hit.entry as usize];
        let shortcut = f & flags::DIR == 0 && Location::from_flags(f) == Location::StartMenu;
        // Same file name inside equally named folders; "Uninstall.lnk" of two apps
        // sits in differently named folders and both stay.
        let key = (
            index.name_ids()[hit.entry as usize],
            index
                .parent(hit.entry)
                .map(|p| index.name_ids()[p as usize]),
        );
        !shortcut || seen.insert(key)
    });
    result.total_matches -= before - result.hits.len();
    result
}

/// Pass 1: one score per unique name; 0 means no match. With `candidates`, only those
/// names are checked and all others score 0.
fn score_names(names: &NameTable, query: &Query, candidates: Option<&[u32]>) -> NameScores {
    let mut scores = vec![0u8; names.len()];
    let mut masks = if query.partial {
        vec![0u8; names.len()]
    } else {
        Vec::new()
    };
    if let Some(candidates) = candidates {
        let scored: Vec<(u32, (u8, u8))> = candidates
            .par_iter()
            .with_min_len(4096)
            .map_init(Vec::new, |buf, &id| (id, score_name(names, id, buf, query)))
            .collect();
        for (id, (score, mask)) in scored {
            scores[id as usize] = score;
            if let Some(m) = masks.get_mut(id as usize) {
                *m = mask;
            }
        }
        return NameScores { scores, masks };
    }

    const NAMES_PER_CHUNK: usize = 1 << 14;
    let score_chunk = |buf: &mut Vec<u8>, chunk: usize, out: &mut [u8], mask_out: &mut [u8]| {
        let first = chunk * NAMES_PER_CHUNK;
        if query.ascii {
            if query.partial {
                // A name may hold any one of the terms, so each one is scanned for.
                for finder in &query.terms {
                    scan_chunk(names, first, out, mask_out, query, finder, buf);
                }
            } else {
                let finder = &query.terms[query.scan_term];
                scan_chunk(names, first, out, mask_out, query, finder, buf);
            }
        } else {
            for (k, score) in out.iter_mut().enumerate() {
                let (s, m) = score_name(names, (first + k) as u32, buf, query);
                *score = s;
                if let Some(slot) = mask_out.get_mut(k) {
                    *slot = m;
                }
            }
        }
    };
    if query.partial {
        scores
            .par_chunks_mut(NAMES_PER_CHUNK)
            .zip(masks.par_chunks_mut(NAMES_PER_CHUNK))
            .enumerate()
            .for_each_init(Vec::new, |buf, (chunk, (out, mask_out))| {
                score_chunk(buf, chunk, out, mask_out)
            });
    } else {
        scores
            .par_chunks_mut(NAMES_PER_CHUNK)
            .enumerate()
            .for_each_init(Vec::new, |buf, (chunk, out)| {
                score_chunk(buf, chunk, out, &mut [])
            });
    }
    NameScores { scores, masks }
}

/// Scans `out.len()` consecutive names starting at `first` as one block of bytes and
/// scores the names that contain `finder` and are not scored yet. `masks` is empty
/// unless the query keeps partial matches.
fn scan_chunk(
    names: &NameTable,
    first: usize,
    out: &mut [u8],
    masks: &mut [u8],
    query: &Query,
    finder: &Finder<'_>,
    buf: &mut Vec<u8>,
) {
    let offsets = &names.offsets()[first..=first + out.len()];
    let base = offsets[0] as usize;
    let hay = &names.folded_buffer()[base..offsets[out.len()] as usize];
    let needle_len = finder.needle().len();
    let mut local = 0usize;
    let mut pos = 0usize;
    while pos < hay.len() {
        let Some(found) = finder.find(&hay[pos..]) else {
            break;
        };
        let p = pos + found;
        while offsets[local + 1] as usize - base <= p {
            local += 1;
        }
        let name_end = offsets[local + 1] as usize - base;
        if p + needle_len <= name_end {
            // Every scored name has a non-zero score, so 0 means "not yet".
            if out[local] == 0 {
                let (score, mask) = score_name(names, (first + local) as u32, buf, query);
                out[local] = score;
                if let Some(slot) = masks.get_mut(local) {
                    *slot = mask;
                }
            }
            pos = name_end;
        } else {
            // The hit runs into the next name.
            pos = p + 1;
        }
    }
}

/// Entries in system, app-data and program folders: places people rarely look in.
fn is_system(entry_flags: u8) -> bool {
    matches!(
        Location::from_flags(entry_flags),
        Location::Noisy | Location::AppFiles
    )
}

fn final_score(name_score: u8, entry_flags: u8) -> i32 {
    let mut score = i32::from(name_score);
    score += match Location::from_flags(entry_flags) {
        Location::UserContent => 25,
        Location::Normal => 0,
        Location::Noisy => -45,
        Location::AppFiles => -30,
        // Shortcuts are the entries that launch installed programs. The folders that
        // group them are ordinary folders.
        Location::StartMenu if entry_flags & flags::DIR != 0 => 0,
        Location::StartMenu => 60,
    };
    // Folders rank below documents and media but above code and unknown files.
    if entry_flags & flags::DIR != 0 {
        score += 6;
    }
    // Folders are left out so a program's own folder never outranks its shortcut, and
    // so are exact launchable names, which already have their own boost.
    if (EXACT_STEM..APP_STEM).contains(&i32::from(name_score)) && entry_flags & flags::DIR == 0 {
        score += EXACT_BONUS;
    }
    if entry_flags & flags::HIDDEN != 0 {
        score -= 20;
    }
    score
}

/// Scores one name against the query: the score (0 when it does not match) and which
/// terms it matched. Without [`Query::partial`], a match needs every term.
fn score_name(names: &NameTable, id: u32, buf: &mut Vec<u8>, query: &Query) -> (u8, u8) {
    let ascii = query.ascii;
    let lower: &[u8] = if ascii {
        names.folded(id)
    } else {
        // Non-ASCII query: full Unicode lowercasing, which can change byte lengths, so
        // the lowercased copy is used for matching and word starts (camelCase is not
        // detected).
        buf.clear();
        buf.extend_from_slice(names.get(id).to_lowercase().as_bytes());
        buf
    };
    if !query.extension_ok(lower) {
        return (0, 0);
    }
    let start = names.range(id).start;
    let upper = |i: usize| ascii && names.is_upper(start + i);
    let mut total = 0;
    let mut mask = 0u8;
    let mut matched_len = 0;
    for (i, term) in query.terms.iter().enumerate() {
        let needle = term.needle();
        let mut best = 0;
        if needle.len() <= lower.len() {
            for pos in memchr::memchr_iter(needle[0], &lower[..=lower.len() - needle.len()]) {
                if &lower[pos..pos + needle.len()] == needle {
                    best = best.max(classify(lower, pos, needle.len(), upper));
                    if best >= PREFIX {
                        break;
                    }
                }
            }
        }
        if best == 0 {
            if !query.partial {
                return (0, 0);
            }
            total += FOLDER_TERM;
        } else {
            total += best;
            mask |= 1 << i;
            matched_len += needle.len();
        }
    }
    if mask == 0 {
        return (0, 0);
    }
    (finish_score(total, lower, query, matched_len), mask)
}

/// `lower` is the lowercased name; `upper(i)` tells whether byte `i` was uppercase.
fn classify(lower: &[u8], pos: usize, needle_len: usize, upper: impl Fn(usize) -> bool) -> i32 {
    if pos == 0 {
        return if needle_len == lower.len() {
            EXACT
        } else {
            PREFIX
        };
    }
    let prev = lower[pos - 1];
    let cur = lower[pos];
    let word_start = (prev.is_ascii() && !prev.is_ascii_alphanumeric())
        || (prev.is_ascii_lowercase() && !upper(pos - 1) && cur.is_ascii_lowercase() && upper(pos))
        || (prev.is_ascii_digit() && cur.is_ascii_alphabetic())
        || (prev.is_ascii_alphabetic() && cur.is_ascii_digit());
    if word_start { WORD_START } else { SUBSTRING }
}

/// Turns summed term scores into the final name score. `lower` is the lowercased name
/// and `matched_len` the length of the terms found in it.
fn finish_score(total: i32, lower: &[u8], query: &Query, matched_len: usize) -> u8 {
    let mut score = total / query.terms.len() as i32;

    // The name is exactly the query plus a single extension (`factory.exe`, `notes.txt`).
    let stem_extension = match query.terms.as_slice() {
        [term] if score == PREFIX => {
            let n = term.needle().len();
            if lower.get(n) == Some(&b'.') && !lower[n + 1..].contains(&b'.') {
                Some(&lower[n + 1..])
            } else {
                None
            }
        }
        _ => None,
    };
    let dot = memchr::memrchr(b'.', lower);
    let extension = dot.map(|d| &lower[d + 1..]);

    match stem_extension {
        // A launchable file named exactly after the query is the app the user meant.
        Some(extension) if LAUNCHABLE.contains(&extension) => score = APP_STEM,
        // `notes.log` is not what someone typing "notes" wants, so it gets no exact
        // bonus (that starts at EXACT_STEM) and stays below documents.
        Some(extension) if DEMOTED.contains(&extension) => score = PREFIX - 10,
        Some(extension) => score = EXACT_STEM + kind_bonus(extension).max(0),
        None => {
            if let Some(extension) = extension {
                if LAUNCHABLE.contains(&extension) {
                    // An installer is run once, so it should not outrank the program.
                    if extension == b"msi" || looks_like_installer(lower) {
                        score -= 20;
                    } else {
                        score += APP_BONUS;
                    }
                } else if DEMOTED.contains(&extension) {
                    score -= 10;
                } else {
                    score += kind_bonus(extension);
                }
            }
        }
    }

    // The extension of an exact stem match is not extra text. Document and media
    // names are often long and descriptive, so their length costs less.
    if stem_extension.is_none() {
        let extra = lower.len().saturating_sub(matched_len);
        let gentle = extension.is_some_and(|e| DOCUMENTS.contains(&e) || MEDIA.contains(&e));
        score -= if gentle {
            (extra / 8).min(8)
        } else {
            (extra / 4).min(15)
        } as i32;
    }

    if let (Some(d), Some(extension)) = (dot, extension)
        && (extension == b"lnk" || extension == b"url")
        && is_generic_shortcut(&lower[..d])
    {
        score = score.min(GENERIC_SHORTCUT);
    }
    score.clamp(1, 255) as u8
}

fn looks_like_installer(lower: &[u8]) -> bool {
    ["setup", "install", "unins"]
        .iter()
        .any(|word| memmem::find(lower, word.as_bytes()).is_some())
}

/// (score, shallower first, lower entry first): higher is better.
type Ranked = (i32, Reverse<u16>, Reverse<u32>);

/// Keeps the `limit` best hits. Ties go to the shallower entry, then the lower entry
/// index, so results are deterministic regardless of how work is split across threads.
struct TopK {
    limit: usize,
    heap: BinaryHeap<Reverse<Ranked>>,
    total: usize,
    hidden: usize,
}

impl TopK {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::with_capacity(limit.min(1024) + 1),
            total: 0,
            hidden: 0,
        }
    }

    /// `depth` walks the parents, so it is only called for hits that can still place.
    fn push(&mut self, score: i32, entry: u32, depth: impl FnOnce() -> u16) {
        self.total += 1;
        if self.heap.len() >= self.limit
            && self
                .heap
                .peek()
                .is_none_or(|Reverse(worst)| score < worst.0)
        {
            return;
        }
        self.offer((score, Reverse(depth()), Reverse(entry)));
    }

    fn offer(&mut self, item: Ranked) {
        if self.heap.len() < self.limit {
            self.heap.push(Reverse(item));
        } else if let Some(Reverse(worst)) = self.heap.peek()
            && item > *worst
        {
            self.heap.pop();
            self.heap.push(Reverse(item));
        }
    }

    fn merge(mut self, other: Self) -> Self {
        self.total += other.total;
        self.hidden += other.hidden;
        for Reverse(item) in other.heap {
            self.offer(item);
        }
        self
    }

    fn into_result(self) -> SearchResult {
        let mut items: Vec<_> = self.heap.into_iter().map(|Reverse(item)| item).collect();
        items.sort_unstable_by(|a, b| b.cmp(a));
        SearchResult {
            hits: items
                .into_iter()
                .map(|(score, Reverse(depth), Reverse(entry))| Hit {
                    entry,
                    score,
                    depth,
                })
                .collect(),
            total_matches: self.total,
            hidden_matches: self.hidden,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bs_index::{Change, IndexBuilder};

    fn index_of(paths: &[&str]) -> Index {
        // Builds folders on demand from backslash-separated paths under "T:".
        let mut b = IndexBuilder::new();
        b.begin_volume("T:", 0);
        let mut known = std::collections::HashMap::new();
        let mut next = 1u64;
        for path in paths {
            let parts: Vec<&str> = path.split('\\').collect();
            let mut parent = 0u64;
            let mut prefix = String::new();
            for (i, part) in parts.iter().enumerate() {
                prefix.push('\\');
                prefix.push_str(part);
                if let Some(&id) = known.get(&prefix) {
                    parent = id;
                    continue;
                }
                b.push(next, parent, part, i + 1 < parts.len(), false);
                known.insert(prefix.clone(), next);
                parent = next;
                next += 1;
            }
        }
        b.end_volume();
        b.finish()
    }

    fn paths(index: &Index, query: &str, limit: usize) -> Vec<String> {
        let q = Query::parse(query).unwrap();
        search(index, &q, limit)
            .hits
            .iter()
            .map(|h| index.full_path(h.entry))
            .collect()
    }

    fn naive_scores(names: &NameTable, query: &Query) -> (Vec<u8>, Vec<u8>) {
        let mut buf = Vec::new();
        let (scores, masks): (Vec<u8>, Vec<u8>) = (0..names.len() as u32)
            .map(|id| score_name(names, id, &mut buf, query))
            .unzip();
        (scores, if query.partial { masks } else { Vec::new() })
    }

    #[test]
    fn filter_hides_entries_from_hits_and_count() {
        let index = index_of(&["a\\note.txt", "b\\note.md", "b\\notes"]);
        let q = Query::parse("note").unwrap();
        let hidden: Vec<u32> = (0..index.len() as u32)
            .filter(|&e| index.full_path(e).starts_with("T:\\b\\"))
            .collect();
        let filter = |e: u32| !hidden.contains(&e);
        let result = search_filtered(&index, &q, 10, Some(&filter));
        assert_eq!(result.total_matches, 1);
        assert_eq!(index.full_path(result.hits[0].entry), "T:\\a\\note.txt");

        let mut session = Session::new();
        assert_eq!(session.search(&index, &q, 10).total_matches, 3);
        let narrowed = Query::parse("note.").unwrap();
        let result = session.search_filtered(&index, &narrowed, 10, Some(&filter));
        assert_eq!(result.total_matches, 1);
    }

    #[test]
    fn ranks_exact_then_prefix_then_word_then_substring() {
        let index = index_of(&[
            "dir\\report",
            "dir\\reports2020",
            "dir\\my_report_x",
            "dir\\unreported",
        ]);
        assert_eq!(
            paths(&index, "report", 10),
            vec![
                "T:\\dir\\report",
                "T:\\dir\\reports2020",
                "T:\\dir\\my_report_x",
                "T:\\dir\\unreported"
            ]
        );
    }

    #[test]
    fn kinds_rank_apps_then_documents_then_media_then_folders_then_code() {
        let index = index_of(&[
            "d\\budget.rs",
            "d\\budget_folder\\x",
            "d\\budget_song.mp3",
            "d\\budget_plan.pdf",
            "d\\budget_app.exe",
        ]);
        let names: Vec<String> = paths(&index, "budget_", 10)
            .into_iter()
            .filter(|p| p.matches('\\').count() == 2)
            .collect();
        assert_eq!(
            names,
            vec![
                "T:\\d\\budget_app.exe",
                "T:\\d\\budget_plan.pdf",
                "T:\\d\\budget_song.mp3",
                "T:\\d\\budget_folder",
            ]
        );
    }

    #[test]
    fn duplicate_start_menu_shortcuts_are_listed_once() {
        let all_users = "ProgramData\\Microsoft\\Windows\\Start Menu\\Programs";
        let user = "Users\\bob\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs";
        let index = index_of(&[
            &format!("{all_users}\\Access.lnk"),
            &format!("{user}\\Access.lnk"),
            &format!("{all_users}\\Foo\\Uninstall.lnk"),
            &format!("{all_users}\\Bar\\Uninstall.lnk"),
        ]);
        let q = Query::parse("access").unwrap();
        let result = search(&index, &q, 10);
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.total_matches, 1);
        let q = Query::parse("uninstall").unwrap();
        assert_eq!(search(&index, &q, 10).hits.len(), 2);
    }

    #[test]
    fn exact_name_beats_the_app_preference() {
        let index = index_of(&["d\\ac.pdf", "d\\access.exe"]);
        assert_eq!(paths(&index, "ac", 10)[0], "T:\\d\\ac.pdf");
    }

    #[test]
    fn exact_name_without_extension_ranks_above_longer_prefix() {
        let index = index_of(&["a\\notepad_backup.txt", "a\\notepad.exe"]);
        assert_eq!(paths(&index, "notepad", 10)[0], "T:\\a\\notepad.exe");
    }

    #[test]
    fn launchable_app_outranks_folders_and_installers() {
        let index = index_of(&[
            "Users\\hp\\Desktop\\Factory.lnk",
            "Users\\hp\\Downloads\\Factory-0.28.0 Setup.exe",
            "msys64\\share\\zoneinfo\\Factory",
            "Users\\hp\\bin\\factory.exe",
        ]);
        let order = paths(&index, "factory", 10);
        let position = |suffix: &str| order.iter().position(|p| p.ends_with(suffix)).unwrap();
        let app = position("factory.exe");
        assert!(app < position("zoneinfo\\Factory"), "{order:?}");
        assert!(app < position("Setup.exe"), "{order:?}");
        // The Desktop shortcut to the app is a launchable match too, so it leads.
        assert_eq!(order[0], "T:\\Users\\hp\\Desktop\\Factory.lnk");
    }

    #[test]
    fn certificate_and_runtime_types_are_demoted() {
        let index = index_of(&[
            "a\\factory.exe",
            "a\\factory-ai-root.pem",
            "a\\factory-desktop-cdp-25828.pid",
        ]);
        let order = paths(&index, "factory", 10);
        assert_eq!(order[0], "T:\\a\\factory.exe");
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn is_case_insensitive_and_matches_camel_case_word_starts() {
        let index = index_of(&["src\\MyReportViewer.cs", "src\\xreportx.cs"]);
        let q = Query::parse("REPORT").unwrap();
        let result = search(&index, &q, 10);
        assert_eq!(result.total_matches, 2);
        assert_eq!(
            index.full_path(result.hits[0].entry),
            "T:\\src\\MyReportViewer.cs"
        );
    }

    #[test]
    fn uppercase_runs_are_not_camel_case_word_starts() {
        // "PDFViewer": "viewer" follows "F", which is uppercase, so it is a plain substring.
        let index = index_of(&["a\\PDFViewer", "a\\pdfViewer"]);
        let q = Query::parse("viewer").unwrap();
        let hits = search(&index, &q, 10).hits;
        assert_eq!(index.full_path(hits[0].entry), "T:\\a\\pdfViewer");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn all_terms_must_match() {
        let index = index_of(&[
            "x\\budget 2024.xlsx",
            "x\\budget 2023.xlsx",
            "x\\notes 2024.txt",
        ]);
        assert_eq!(
            paths(&index, "budget 2024", 10),
            vec!["T:\\x\\budget 2024.xlsx"]
        );
    }

    #[test]
    fn matches_non_ascii_names() {
        let index = index_of(&["docs\\Résumé.pdf"]);
        assert_eq!(paths(&index, "RÉSUMÉ", 10), vec!["T:\\docs\\Résumé.pdf"]);
        assert_eq!(paths(&index, "sum", 10), vec!["T:\\docs\\Résumé.pdf"]);
    }

    #[test]
    fn demotes_noisy_locations() {
        let index = index_of(&["proj\\node_modules\\lodash\\index.js", "proj\\index.js"]);
        assert_eq!(paths(&index, "index.js", 10)[0], "T:\\proj\\index.js");
    }

    #[test]
    fn demotes_program_internals_and_boosts_start_menu_shortcuts() {
        let index = index_of(&[
            "Program Files\\Acme\\plugins\\Acme Notes.txt",
            "work\\Acme Notes.txt",
        ]);
        assert_eq!(
            paths(&index, "acme notes", 10)[0],
            "T:\\work\\Acme Notes.txt"
        );

        let index = index_of(&[
            "Users\\bob\\Downloads\\Acme Setup.exe",
            "ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Acme Setup.lnk",
        ]);
        assert_eq!(
            paths(&index, "acme setup", 10)[0],
            "T:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Acme Setup.lnk"
        );
    }

    #[test]
    fn hiding_system_folders_counts_them_apart() {
        let index = index_of(&[
            "Users\\bob\\Documents\\notes.txt",
            "Users\\bob\\AppData\\Local\\App\\notes.cache",
            "Program Files\\Acme\\plugins\\data\\notes.md",
            "work\\notes.md",
            "ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Notes.lnk",
        ]);
        let q = Query::parse("notes").unwrap();
        let all = search(&index, &q, 10);
        assert_eq!((all.total_matches, all.hidden_matches), (5, 0));
        let q = Query::parse("notes").unwrap().hiding_system(true);
        let shown = search(&index, &q, 10);
        assert_eq!((shown.total_matches, shown.hidden_matches), (3, 2));
        let found: Vec<String> = shown
            .hits
            .iter()
            .map(|h| index.full_path(h.entry))
            .collect();
        assert!(
            found
                .iter()
                .all(|p| !p.contains("AppData") && !p.contains("Acme"))
        );
        assert!(found.iter().any(|p| p.contains("Start Menu")));
        // Session and fresh searches agree, including after narrowing.
        let mut session = Session::new();
        let q = Query::parse("note").unwrap().hiding_system(true);
        session.search(&index, &q, 10);
        let q = Query::parse("notes").unwrap().hiding_system(true);
        let narrowed = session.search(&index, &q, 10);
        assert_eq!((narrowed.total_matches, narrowed.hidden_matches), (3, 2));
    }

    #[test]
    fn hidden_acronym_matches_are_counted_too() {
        let index = index_of(&[
            "Program Files\\Acme\\lib\\data\\very_serious_charts.txt",
            "docs\\Visual Studio Code.txt",
        ]);
        let q = Query::parse("vsc").unwrap().hiding_system(true);
        let result = search(&index, &q, 10);
        assert_eq!((result.total_matches, result.hidden_matches), (1, 1));
    }

    #[test]
    fn start_menu_shortcut_beats_installer_and_app_folder() {
        let index = index_of(&[
            "Users\\bob\\Downloads\\ChromeSetup.exe",
            "Program Files\\Google\\Chrome\\Application\\chrome.exe",
            "ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Google Chrome.lnk",
            "Windows\\ServiceProfiles\\Svc\\Start Menu\\Programs\\Google Chrome.lnk",
        ]);
        let found = paths(&index, "chrome", 10);
        assert_eq!(
            found[0],
            "T:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Google Chrome.lnk"
        );
        assert_eq!(
            found.last().unwrap(),
            "T:\\Windows\\ServiceProfiles\\Svc\\Start Menu\\Programs\\Google Chrome.lnk"
        );
        // Start Menu folders get no boost of their own.
        let index = index_of(&[
            "ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Git\\Git Bash.lnk",
            "Users\\bob\\Documents\\Git\\readme",
        ]);
        let found = paths(&index, "git", 10);
        assert_eq!(
            found[0],
            "T:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Git\\Git Bash.lnk"
        );
    }

    #[test]
    fn word_initials_find_multi_word_names() {
        let index = index_of(&[
            "ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Visual Studio Code.lnk",
            "docs\\very_serious_charts.txt",
            "docs\\Visual Studios Code Notes.txt",
            "docs\\vscode.md",
            "docs\\Vsc Extra.txt",
            "docs\\Video\\Setup\\Cache",
        ]);
        let found = paths(&index, "vsc", 10);
        // The shortcut leads: its initials match and the Start Menu boost applies.
        assert_eq!(
            found[0],
            "T:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Visual Studio Code.lnk"
        );
        assert!(found.contains(&"T:\\docs\\very_serious_charts.txt".to_string()));
        assert!(found.contains(&"T:\\docs\\vscode.md".to_string()));
        // "Visual Studios Code Notes" spells vscn, not vsc.
        let acronym_only = paths(&index, "vscn", 10);
        assert_eq!(
            acronym_only,
            vec!["T:\\docs\\Visual Studios Code Notes.txt".to_string()]
        );
        // Not an acronym query: several terms, digits, or too short or long.
        assert!(paths(&index, "vsc n", 10).is_empty());
        assert!(paths(&index, "v", 10).len() > 1);
    }

    #[test]
    fn camel_case_words_count_as_initials() {
        let index = index_of(&["src\\MyReportViewer.cs", "src\\Myreportviewer.cs"]);
        assert_eq!(
            paths(&index, "mrv", 10),
            vec!["T:\\src\\MyReportViewer.cs".to_string()]
        );
    }

    #[test]
    fn limit_caps_hits_but_counts_all_matches() {
        let files: Vec<String> = (0..5000).map(|i| format!("d\\file{i}.txt")).collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let index = index_of(&refs);
        let q = Query::parse("file").unwrap();
        let result = search(&index, &q, 20);
        assert_eq!(result.hits.len(), 20);
        assert_eq!(result.total_matches, 5000);
        assert!(result.hits.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn no_match_returns_nothing() {
        let index = index_of(&["a\\b.txt"]);
        let q = Query::parse("zzz").unwrap();
        let result = search(&index, &q, 20);
        assert!(result.hits.is_empty());
        assert_eq!(result.total_matches, 0);
    }

    #[test]
    fn matches_never_span_two_names() {
        let index = index_of(&["d\\ab", "d\\cd"]);
        let q = Query::parse("bc").unwrap();
        assert_eq!(search(&index, &q, 10).total_matches, 0);
    }

    #[test]
    fn skips_deleted_entries() {
        let mut index = index_of(&["d\\old.txt", "d\\new.txt"]);
        index.apply(0, Change::Delete { record: 2 });
        index.end_batch();
        assert_eq!(paths(&index, "txt", 10), vec!["T:\\d\\new.txt"]);
    }

    fn mixed_index() -> Index {
        let words = [
            "Report",
            "photo",
            "INDEX",
            "x",
            "data_2024",
            "my-notes",
            "ée",
            "qz",
        ];
        let files: Vec<String> = (0..40_000)
            .map(|i| {
                let a = words[i % words.len()];
                let b = words[(i / 7) % words.len()];
                format!("f{}\\{a}{b}{}.txt", i % 97, i % 13)
            })
            .collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        index_of(&refs)
    }

    #[test]
    fn buffer_scan_agrees_with_scoring_every_name() {
        let index = mixed_index();
        for text in [
            "e",
            "rep",
            "PHOTO",
            "x data",
            "qz",
            ".txt",
            "tx",
            "otes2",
            "zzz",
            "ée",
            "f1 ée",
            "ext:txt",
            "note ext:txt,md",
        ] {
            let q = Query::parse(text).unwrap();
            let scanned = score_names(index.names(), &q, None);
            assert_eq!(
                (scanned.scores, scanned.masks),
                naive_scores(index.names(), &q),
                "query {text:?}"
            );
        }
    }

    #[test]
    fn session_narrowing_matches_fresh_searches() {
        let index = mixed_index();
        let mut session = Session::new();
        let typed = [
            "r",
            "re",
            "rep",
            "repo",
            "repor",
            "report",
            "report p",
            "report ph",
            "report",
            "x",
            "x d",
            "zz",
        ];
        for text in typed {
            let q = Query::parse(text).unwrap();
            let narrowed = session.search(&index, &q, 25);
            let fresh = search(&index, &q, 25);
            assert_eq!(
                narrowed.total_matches, fresh.total_matches,
                "query {text:?}"
            );
            assert_eq!(narrowed.hits, fresh.hits, "query {text:?}");
        }
    }

    #[test]
    fn long_match_lists_are_not_remembered() {
        let paths: Vec<String> = (0..MAX_NARROW_NAMES + 100)
            .map(|i| format!("keep\\note{i:05}.txt"))
            .collect();
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let index = index_of(&paths);
        let mut session = Session::new();

        let q = Query::parse("note").unwrap();
        assert!(session.search(&index, &q, 5).total_matches > MAX_NARROW_NAMES);
        assert!(session.last.as_ref().unwrap().matched_names.is_none());

        // Narrowing is off, but the results must be the same as a fresh search.
        let q = Query::parse("note9").unwrap();
        let narrowed = session.search(&index, &q, 5);
        let fresh = search(&index, &q, 5);
        assert_eq!(narrowed.total_matches, fresh.total_matches);
        assert_eq!(narrowed.hits, fresh.hits);
        assert!(session.last.as_ref().unwrap().matched_names.is_some());
    }

    #[test]
    fn session_starts_over_when_index_changes() {
        let mut index = index_of(&["d\\alpha.txt"]);
        let mut session = Session::new();
        let q = Query::parse("al").unwrap();
        assert_eq!(session.search(&index, &q, 10).total_matches, 1);
        index.apply(
            0,
            Change::Upsert {
                record: 50,
                parent_record: 1,
                name: "alpine.txt",
                is_dir: false,
                hidden: false,
            },
        );
        index.end_batch();
        let q = Query::parse("alp").unwrap();
        assert_eq!(session.search(&index, &q, 10).total_matches, 2);
    }

    #[test]
    fn narrowing_rule() {
        let last = |terms: &[&str], extensions: &[&str]| LastSearch {
            generation: 0,
            terms: terms.iter().map(|t| t.as_bytes().to_vec()).collect(),
            extensions: extensions.iter().map(|e| e.as_bytes().to_vec()).collect(),
            matched_names: None,
        };
        let q = Query::parse("report").unwrap();
        assert!(q.narrows(&last(&["rep"], &[])));
        assert!(!q.narrows(&last(&["reports"], &[])));
        assert!(!q.narrows(&last(&["rep"], &["pdf"])));
        // Several terms keep names that match only some of them, so each new term
        // must imply an old one.
        let q = Query::parse("report 2024").unwrap();
        assert!(q.narrows(&last(&["rep", "202"], &[])));
        assert!(q.narrows(&last(&["202", "port"], &[])));
        assert!(!q.narrows(&last(&["rep"], &[])));
        assert!(!q.narrows(&last(&["x"], &[])));
        let q = Query::parse("report ext:pdf").unwrap();
        assert!(q.narrows(&last(&["rep"], &["pdf"])));
        assert!(!q.narrows(&last(&["rep"], &[])));
    }

    #[test]
    fn missing_terms_may_match_parent_folders() {
        let index = index_of(&[
            "Users\\bob\\Documents\\Clients\\Acme\\Contract.pdf",
            "Users\\bob\\Documents\\Clients\\Other\\Contract.pdf",
            "Users\\bob\\Documents\\Acme Contract.pdf",
        ]);
        let found = paths(&index, "acme contract", 10);
        assert_eq!(
            found,
            vec![
                "T:\\Users\\bob\\Documents\\Acme Contract.pdf",
                "T:\\Users\\bob\\Documents\\Clients\\Acme\\Contract.pdf",
            ]
        );
        // Every term must still be found, and the volume root does not count.
        assert!(paths(&index, "zebra contract", 10).is_empty());
        assert!(paths(&index, "t: contract", 10).is_empty());
        // A term matching only folders does not list the folder's whole contents.
        assert!(
            paths(&index, "acme clients", 10)
                .iter()
                .all(|p| p.ends_with("Acme"))
        );
        // Typing narrows the same way as a fresh search.
        let mut session = Session::new();
        for text in [
            "a",
            "ac",
            "acme",
            "acme c",
            "acme co",
            "acme contract",
            "acme",
        ] {
            let q = Query::parse(text).unwrap();
            let narrowed = session.search(&index, &q, 10);
            let fresh = search(&index, &q, 10);
            assert_eq!(narrowed.hits, fresh.hits, "query {text:?}");
            assert_eq!(
                narrowed.total_matches, fresh.total_matches,
                "query {text:?}"
            );
        }
    }

    #[test]
    fn extension_filter() {
        let index = index_of(&[
            "d\\budget.xlsx",
            "d\\budget.pdf",
            "d\\budget.docx",
            "d\\report.pdf",
        ]);
        assert_eq!(
            paths(&index, "budget ext:pdf", 10),
            vec!["T:\\d\\budget.pdf"]
        );
        let mut found = paths(&index, "EXT:.pdf", 10);
        found.sort();
        assert_eq!(found, vec!["T:\\d\\budget.pdf", "T:\\d\\report.pdf"]);
        assert_eq!(paths(&index, "budget ext:xlsx,docx", 10).len(), 2);
        assert!(Query::parse("ext:").is_none());
    }

    #[test]
    fn runtime_files_named_like_the_query_stay_below_documents() {
        let index = index_of(&[
            "Users\\bob\\Documents\\notes.log",
            "Users\\bob\\Documents\\server.pem",
            "Users\\bob\\Documents\\Notes 2024 meeting.docx",
            "Users\\bob\\Documents\\server notes.docx",
        ]);
        assert!(paths(&index, "notes", 10)[0].ends_with("Notes 2024 meeting.docx"));
        assert!(paths(&index, "server", 10)[0].ends_with("server notes.docx"));
    }

    #[test]
    fn generic_start_menu_shortcuts_stay_below_user_files() {
        let menu = "Users\\bob\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs";
        let index = index_of(&[
            &format!("{menu}\\Acme\\Readme.lnk"),
            &format!("{menu}\\Acme\\Acme Website.url"),
            &format!("{menu}\\Acme\\Acme.lnk"),
            "Users\\bob\\Documents\\readme for taxes.txt",
            "Users\\bob\\Documents\\acme website notes.txt",
        ]);
        assert!(paths(&index, "readme", 10)[0].ends_with("readme for taxes.txt"));
        let found = paths(&index, "acme", 10);
        assert!(found[0].ends_with("Acme.lnk"), "{found:?}");
        assert!(paths(&index, "acme website", 10)[0].ends_with("notes.txt"));
    }

    #[test]
    fn equal_matches_prefer_shallower_paths() {
        let index = index_of(&["a\\b\\c\\d\\lib.rs", "x\\lib.rs", "a\\b\\lib.rs"]);
        assert_eq!(
            paths(&index, "lib.rs", 10),
            vec![
                "T:\\x\\lib.rs",
                "T:\\a\\b\\lib.rs",
                "T:\\a\\b\\c\\d\\lib.rs"
            ]
        );
    }

    #[test]
    fn empty_query_is_rejected() {
        assert!(Query::parse("   ").is_none());
    }
}
