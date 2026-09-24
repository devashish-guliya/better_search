//! Ranked, case-insensitive file name search over a [`bs_index::Index`].
//!
//! The search runs in two parallel passes:
//! 1. Score every *unique* name once (repeated names like `index.js` are checked once).
//!    For ASCII queries this does not visit names one by one: it scans the packed name
//!    buffer for the query's rarest byte with SIMD and only scores names around hits.
//! 2. Walk all entries, look up the score of their name, adjust it by location, and keep
//!    only the best `limit` hits per thread. The full match list is never materialized,
//!    so memory stays small even when millions of entries match.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bs_index::{Index, Location, NameTable, flags};
use memchr::memmem::Finder;
use rayon::prelude::*;

/// A parsed query: whitespace-separated terms that must all appear in the name.
pub struct Query {
    terms: Vec<Finder<'static>>,
    needle_len: usize,
    ascii: bool,
    /// Present when every term is ASCII, enabling the fast buffer scan.
    anchor: Option<Anchor>,
}

/// The term byte used to find candidate names quickly.
struct Anchor {
    term: usize,
    /// Position of the rare byte inside the term.
    offset: usize,
    lower: u8,
    upper: u8,
}

/// Bytes ordered from most to least common in file names; later means rarer.
const COMMON_BYTES: &[u8] = b"ea.itonsrlcdmpg_u-hb 1f20y3v4w5k6789xjqz";

fn rarity(b: u8) -> usize {
    let b = b.to_ascii_lowercase();
    COMMON_BYTES
        .iter()
        .position(|&c| c == b)
        .unwrap_or(COMMON_BYTES.len())
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
        let anchor = if ascii {
            terms
                .iter()
                .enumerate()
                .flat_map(|(term, f)| {
                    f.needle()
                        .iter()
                        .enumerate()
                        .map(move |(offset, &b)| (term, offset, b))
                })
                .max_by_key(|&(_, _, b)| rarity(b))
                .map(|(term, offset, b)| Anchor {
                    term,
                    offset,
                    lower: b.to_ascii_lowercase(),
                    upper: b.to_ascii_uppercase(),
                })
        } else {
            None
        };
        Some(Self {
            terms,
            needle_len,
            ascii,
            anchor,
        })
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

const EXACT: i32 = 100;
const EXACT_STEM: i32 = 90;
const PREFIX: i32 = 70;
const WORD_START: i32 = 50;
const SUBSTRING: i32 = 30;

pub fn search(index: &Index, query: &Query, limit: usize) -> SearchResult {
    let name_scores = score_names(index.names(), query);

    if name_scores.iter().all(|&s| s == 0) {
        return SearchResult {
            hits: Vec::new(),
            total_matches: 0,
        };
    }

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
                    if name_score != 0 {
                        let entry = base + k as u32;
                        // Volume roots ("C:") are not useful results.
                        if index.parent(entry).is_some() {
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

/// Returns one score per unique name; 0 means no match.
fn score_names(names: &NameTable, query: &Query) -> Vec<u8> {
    const NAMES_PER_CHUNK: usize = 1 << 14;
    let mut scores = vec![0u8; names.len()];
    scores
        .par_chunks_mut(NAMES_PER_CHUNK)
        .enumerate()
        .for_each_init(
            || Vec::with_capacity(256),
            |buf, (chunk, out)| {
                let first = chunk * NAMES_PER_CHUNK;
                match &query.anchor {
                    Some(anchor) => scan_chunk(names, first, out, query, anchor, buf),
                    None => {
                        for (k, score) in out.iter_mut().enumerate() {
                            *score = score_name(names.bytes((first + k) as u32), buf, query);
                        }
                    }
                }
            },
        );
    scores
}

/// Finds candidate names in `out.len()` consecutive names starting at `first` by jumping
/// between occurrences of the anchor byte, then fully scores only those candidates.
fn scan_chunk(
    names: &NameTable,
    first: usize,
    out: &mut [u8],
    query: &Query,
    anchor: &Anchor,
    buf: &mut Vec<u8>,
) {
    let offsets = &names.offsets()[first..=first + out.len()];
    let base = offsets[0] as usize;
    let hay = &names.buffer()[base..offsets[out.len()] as usize];
    let needle = query.terms[anchor.term].needle();
    let mut local = 0usize;
    let mut pos = 0usize;
    while pos < hay.len() {
        let found = if anchor.lower == anchor.upper {
            memchr::memchr(anchor.lower, &hay[pos..])
        } else {
            memchr::memchr2(anchor.lower, anchor.upper, &hay[pos..])
        };
        let Some(found) = found else { break };
        let p = pos + found;
        while offsets[local + 1] as usize - base <= p {
            local += 1;
        }
        let name_start = offsets[local] as usize - base;
        let name_end = offsets[local + 1] as usize - base;
        let candidate = p
            .checked_sub(anchor.offset)
            .filter(|&s| s >= name_start && s + needle.len() <= name_end)
            .is_some_and(|s| hay[s..s + needle.len()].eq_ignore_ascii_case(needle));
        if candidate {
            out[local] = score_name(&hay[name_start..name_end], buf, query);
            pos = name_end;
        } else {
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
fn score_name(name: &[u8], lower: &mut Vec<u8>, query: &Query) -> u8 {
    let mut total = 0;
    if query.ascii && name.is_ascii() {
        // ASCII name and query: compare case-insensitively in place, no lowercase copy.
        for term in &query.terms {
            let needle = term.needle();
            let mut best = 0;
            if needle.len() <= name.len() {
                for pos in 0..=name.len() - needle.len() {
                    if name[pos].eq_ignore_ascii_case(&needle[0])
                        && name[pos..pos + needle.len()].eq_ignore_ascii_case(needle)
                    {
                        best = best.max(classify(name, pos, needle.len()));
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
        return finish_score(total, name, query);
    }

    // Unicode lowercasing can change byte lengths, so the lowercased copy is used both
    // for matching and for the word-start check.
    lower.clear();
    match std::str::from_utf8(name) {
        Ok(s) => lower.extend_from_slice(s.to_lowercase().as_bytes()),
        Err(_) => lower.extend(name.iter().map(u8::to_ascii_lowercase)),
    }
    for term in &query.terms {
        let mut best = 0;
        for pos in term.find_iter(lower) {
            best = best.max(classify(lower, pos, term.needle().len()));
            if best >= PREFIX {
                break;
            }
        }
        if best == 0 {
            return 0;
        }
        total += best;
    }
    finish_score(total, lower, query)
}

fn classify(name: &[u8], pos: usize, needle_len: usize) -> i32 {
    if pos == 0 {
        if needle_len == name.len() {
            EXACT
        } else {
            PREFIX
        }
    } else if is_word_start(name, pos) {
        WORD_START
    } else {
        SUBSTRING
    }
}

/// Turns summed term scores into the final name score. `name` may be in any case.
fn finish_score(total: i32, name: &[u8], query: &Query) -> u8 {
    let mut score = total / query.terms.len() as i32;

    if let [term] = query.terms.as_slice() {
        let n = term.needle().len();
        if score == PREFIX && name.get(n) == Some(&b'.') && !name[n + 1..].contains(&b'.') {
            score = EXACT_STEM;
        }
    }

    if let Some(dot) = memchr::memrchr(b'.', name) {
        let ext = &name[dot + 1..];
        let is = |candidates: &[&[u8]]| candidates.iter().any(|c| ext.eq_ignore_ascii_case(c));
        if is(&[b"exe", b"lnk", b"url", b"appref-ms"]) {
            score += 10;
        } else if is(&[
            b"dll",
            b"mui",
            b"tmp",
            b"log",
            b"etl",
            b"cat",
            b"manifest",
            b"pf",
            b"pyc",
        ]) {
            score -= 10;
        }
    }

    let extra = name.len().saturating_sub(query.needle_len);
    score -= (extra / 4).min(15) as i32;
    score.clamp(1, 255) as u8
}

fn is_word_start(name: &[u8], pos: usize) -> bool {
    let prev = name[pos - 1];
    let cur = name[pos];
    (prev.is_ascii() && !prev.is_ascii_alphanumeric())
        || (prev.is_ascii_lowercase() && cur.is_ascii_uppercase())
        || (prev.is_ascii_digit() && cur.is_ascii_alphabetic())
        || (prev.is_ascii_alphabetic() && cur.is_ascii_digit())
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
    use bs_index::IndexBuilder;

    fn index_of(paths: &[&str]) -> Index {
        // Builds folders on demand from backslash-separated paths under "T:".
        let mut b = IndexBuilder::new();
        b.begin_volume("T:", 0);
        let mut known: Vec<(String, u64)> = Vec::new();
        let mut next = 1u64;
        for path in paths {
            let parts: Vec<&str> = path.split('\\').collect();
            let mut parent = 0u64;
            let mut prefix = String::new();
            for (i, part) in parts.iter().enumerate() {
                prefix.push('\\');
                prefix.push_str(part);
                let is_dir = i + 1 < parts.len();
                if let Some((_, id)) = known.iter().find(|(p, _)| *p == prefix) {
                    parent = *id;
                    continue;
                }
                b.push(next, parent, part, is_dir, false);
                known.push((prefix.clone(), next));
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
    fn fast_scan_agrees_with_per_name_scoring() {
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
        let index = index_of(&refs);
        for text in [
            "e", "rep", "PHOTO", "x data", "qz", ".txt", "tx", "otes2", "zzz",
        ] {
            let fast = Query::parse(text).unwrap();
            assert!(fast.anchor.is_some());
            let mut slow = Query::parse(text).unwrap();
            slow.anchor = None;
            assert_eq!(
                score_names(index.names(), &fast),
                score_names(index.names(), &slow),
                "query {text:?}"
            );
        }
    }

    #[test]
    fn empty_query_is_rejected() {
        assert!(Query::parse("   ").is_none());
    }
}
