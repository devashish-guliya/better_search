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

/// A parsed query: whitespace-separated terms that must all appear in the name.
pub struct Query {
    terms: Vec<Finder<'static>>,
    needle_len: usize,
    /// All terms are ASCII, so names can be compared in their stored lowercased form.
    ascii: bool,
    /// Term used to scan the name buffer: the longest, as it produces the fewest hits.
    scan_term: usize,
}

impl Query {
    pub fn parse(input: &str) -> Option<Self> {
        let terms: Vec<Finder<'static>> = input
            .split_whitespace()
            .map(|t| Finder::new(t.to_lowercase().as_bytes()).into_owned())
            .collect();
        if terms.is_empty() {
            return None;
        }
        let needle_len = terms.iter().map(|f| f.needle().len()).sum();
        let ascii = terms.iter().all(|t| t.needle().is_ascii());
        let scan_term = (0..terms.len())
            .max_by_key(|&i| terms[i].needle().len())
            .unwrap_or(0);
        Some(Self {
            terms,
            needle_len,
            ascii,
            scan_term,
        })
    }

    /// True when every name matching `self` also matches a query made of `previous`
    /// terms: each previous term is contained in one of ours.
    fn narrows(&self, previous: &[Vec<u8>]) -> bool {
        previous.iter().all(|old| {
            self.terms
                .iter()
                .any(|new| memmem::find(new.needle(), old).is_some())
        })
    }

    fn term_bytes(&self) -> Vec<Vec<u8>> {
        self.terms.iter().map(|t| t.needle().to_vec()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub entry: u32,
    pub score: i32,
}

pub struct SearchResult {
    /// Best hits, highest score first.
    pub hits: Vec<Hit>,
    /// Number of entries that matched, including those not in `hits`.
    pub total_matches: usize,
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
    if collect_matches(&scores, None, false).0 == 0 {
        return SearchResult {
            hits: Vec::new(),
            total_matches: 0,
        };
    }
    rank(index, &scores, limit, filter)
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
            .filter(|last| last.generation == index.generation() && query.narrows(&last.terms))
            .and_then(|last| last.matched_names.as_deref());
        let scores = score_names(index.names(), query, candidates);
        let (matched, stored) = collect_matches(&scores, candidates, true);
        let result = if matched == 0 {
            SearchResult {
                hits: Vec::new(),
                total_matches: 0,
            }
        } else {
            rank(index, &scores, limit, filter)
        };
        self.last = Some(LastSearch {
            generation: index.generation(),
            terms: query.term_bytes(),
            matched_names: stored,
        });
        result
    }
}

const EXACT: i32 = 100;
/// A launchable file named exactly after the query (`factory.exe`). This is the app
/// the user meant, so it outranks folders and installers that merely share a prefix.
const APP_STEM: i32 = 120;
const EXACT_STEM: i32 = 90;
const PREFIX: i32 = 70;
const WORD_START: i32 = 50;
const SUBSTRING: i32 = 30;

/// Extensions the user launches: programs and shortcuts.
const LAUNCHABLE: &[&[u8]] = &[
    b"exe",
    b"com",
    b"bat",
    b"cmd",
    b"msi",
    b"lnk",
    b"url",
    b"appref-ms",
];

/// Extensions that are almost never the file someone searched for by name: binaries,
/// runtime files, logs and certificates.
const DEMOTED: &[&[u8]] = &[
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
    name_scores: &[u8],
    limit: usize,
    filter: Option<Filter<'_>>,
) -> SearchResult {
    const CHUNK: usize = 1 << 14;
    let name_ids = index.name_ids();
    let entry_flags = index.flags();
    let top = name_ids
        .par_chunks(CHUNK)
        .zip(entry_flags.par_chunks(CHUNK))
        .enumerate()
        .fold(
            || TopK::new(limit),
            |mut top, (chunk, (ids, fl))| {
                let base = (chunk * CHUNK) as u32;
                for (k, (&name_id, &f)) in ids.iter().zip(fl).enumerate() {
                    let name_score = name_scores[name_id as usize];
                    if name_score != 0 && f & flags::DELETED == 0 {
                        let entry = base + k as u32;
                        // Volume roots ("C:") are not useful results.
                        if index.parent(entry).is_some() && filter.is_none_or(|f| f(entry)) {
                            top.push(final_score(name_score, f), entry);
                        }
                    }
                }
                top
            },
        )
        .reduce(|| TopK::new(limit), TopK::merge);
    top.into_result()
}

/// Pass 1: one score per unique name; 0 means no match. With `candidates`, only those
/// names are checked and all others score 0.
fn score_names(names: &NameTable, query: &Query, candidates: Option<&[u32]>) -> Vec<u8> {
    let mut scores = vec![0u8; names.len()];
    if let Some(candidates) = candidates {
        let scored: Vec<(u32, u8)> = candidates
            .par_iter()
            .with_min_len(4096)
            .map_init(Vec::new, |buf, &id| (id, score_name(names, id, buf, query)))
            .collect();
        for (id, score) in scored {
            scores[id as usize] = score;
        }
        return scores;
    }

    const NAMES_PER_CHUNK: usize = 1 << 14;
    scores
        .par_chunks_mut(NAMES_PER_CHUNK)
        .enumerate()
        .for_each_init(Vec::new, |buf, (chunk, out)| {
            let first = chunk * NAMES_PER_CHUNK;
            if query.ascii {
                scan_chunk(names, first, out, query, buf);
            } else {
                for (k, score) in out.iter_mut().enumerate() {
                    *score = score_name(names, (first + k) as u32, buf, query);
                }
            }
        });
    scores
}

/// Scans `out.len()` consecutive names starting at `first` as one block of bytes and
/// scores only the names that contain the scan term.
fn scan_chunk(names: &NameTable, first: usize, out: &mut [u8], query: &Query, buf: &mut Vec<u8>) {
    let offsets = &names.offsets()[first..=first + out.len()];
    let base = offsets[0] as usize;
    let hay = &names.folded_buffer()[base..offsets[out.len()] as usize];
    let finder = &query.terms[query.scan_term];
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
            out[local] = score_name(names, (first + local) as u32, buf, query);
            pos = name_end;
        } else {
            // The hit runs into the next name.
            pos = p + 1;
        }
    }
}

fn final_score(name_score: u8, entry_flags: u8) -> i32 {
    let mut score = i32::from(name_score);
    score += match Location::from_flags(entry_flags) {
        Location::UserContent => 25,
        Location::Normal => 0,
        Location::Noisy => -45,
    };
    if entry_flags & flags::DIR != 0 {
        score += 3;
    }
    if entry_flags & flags::HIDDEN != 0 {
        score -= 20;
    }
    score
}

/// Scores one name against the query. Returns 0 when it does not match.
fn score_name(names: &NameTable, id: u32, buf: &mut Vec<u8>, query: &Query) -> u8 {
    let mut total = 0;
    if query.ascii {
        let folded = names.folded(id);
        let start = names.range(id).start;
        let upper = |i: usize| names.is_upper(start + i);
        for term in &query.terms {
            let needle = term.needle();
            let mut best = 0;
            if needle.len() <= folded.len() {
                for pos in memchr::memchr_iter(needle[0], &folded[..=folded.len() - needle.len()]) {
                    if &folded[pos..pos + needle.len()] == needle {
                        best = best.max(classify(folded, pos, needle.len(), upper));
                        if best >= PREFIX {
                            break;
                        }
                    }
                }
            }
            if best == 0 {
                return 0;
            }
            total += best;
        }
        return finish_score(total, folded, query);
    }

    // Non-ASCII query: full Unicode lowercasing, which can change byte lengths, so the
    // lowercased copy is used for matching and word starts (camelCase is not detected).
    buf.clear();
    buf.extend_from_slice(names.get(id).to_lowercase().as_bytes());
    for term in &query.terms {
        let mut best = 0;
        for pos in term.find_iter(buf) {
            best = best.max(classify(buf, pos, term.needle().len(), |_| false));
            if best >= PREFIX {
                break;
            }
        }
        if best == 0 {
            return 0;
        }
        total += best;
    }
    finish_score(total, buf, query)
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

/// Turns summed term scores into the final name score. `lower` is the lowercased name.
fn finish_score(total: i32, lower: &[u8], query: &Query) -> u8 {
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

    match stem_extension {
        // A launchable file named exactly after the query is the app the user meant.
        Some(extension) if LAUNCHABLE.contains(&extension) => score = APP_STEM,
        Some(_) => score = EXACT_STEM,
        None => {
            if let Some(dot) = memchr::memrchr(b'.', lower) {
                let extension = &lower[dot + 1..];
                if LAUNCHABLE.contains(&extension) {
                    score += 10;
                } else if DEMOTED.contains(&extension) {
                    score -= 10;
                }
            }
        }
    }

    let extra = lower.len().saturating_sub(query.needle_len);
    score -= (extra / 4).min(15) as i32;
    score.clamp(1, 255) as u8
}

/// Keeps the `limit` best hits. Ties go to the lower entry index so results are
/// deterministic regardless of how work is split across threads.
struct TopK {
    limit: usize,
    heap: BinaryHeap<Reverse<(i32, Reverse<u32>)>>,
    total: usize,
}

impl TopK {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::with_capacity(limit.min(1024) + 1),
            total: 0,
        }
    }

    fn push(&mut self, score: i32, entry: u32) {
        self.total += 1;
        self.offer((score, Reverse(entry)));
    }

    fn offer(&mut self, item: (i32, Reverse<u32>)) {
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
                .map(|(score, Reverse(entry))| Hit { entry, score })
                .collect(),
            total_matches: self.total,
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

    fn naive_scores(names: &NameTable, query: &Query) -> Vec<u8> {
        let mut buf = Vec::new();
        (0..names.len() as u32)
            .map(|id| score_name(names, id, &mut buf, query))
            .collect()
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
            "e", "rep", "PHOTO", "x data", "qz", ".txt", "tx", "otes2", "zzz", "ée",
        ] {
            let q = Query::parse(text).unwrap();
            assert_eq!(
                score_names(index.names(), &q, None),
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
        let q = Query::parse("report 2024").unwrap();
        assert!(q.narrows(&[b"rep".to_vec()]));
        assert!(q.narrows(&[b"202".to_vec(), b"port".to_vec()]));
        assert!(!q.narrows(&[b"reports".to_vec()]));
        assert!(!q.narrows(&[b"x".to_vec()]));
    }

    #[test]
    fn empty_query_is_rejected() {
        assert!(Query::parse("   ").is_none());
    }
}
