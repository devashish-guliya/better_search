//! What the user opened, kept per user in `%LOCALAPPDATA%\better_search\history.tsv`.
//! It only reorders results the service already returned, so the pipe protocol, the
//! index and the service never see it.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Entries kept. The weakest are dropped when the list grows past `MAX_ENTRIES`.
const MAX_ENTRIES: usize = 2000;
const KEEP_ENTRIES: usize = 1500;
/// An open counts half as much after this many days.
const HALF_LIFE_DAYS: f64 = 14.0;
/// The largest score bonus history can give. Name matches stay more important.
const MAX_BOOST: i32 = 30;
/// Weights below this are forgotten.
const FORGET_BELOW: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Entry {
    /// Opens, each fading with age, as of `last`.
    weight: f64,
    /// Unix seconds.
    last: u64,
}

impl Entry {
    fn weight_at(&self, now: u64) -> f64 {
        let days = now.saturating_sub(self.last) as f64 / 86_400.0;
        self.weight * 0.5f64.powf(days / HALF_LIFE_DAYS)
    }
}

#[derive(Default)]
pub struct History {
    entries: HashMap<String, Entry>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|dir| PathBuf::from(dir).join("better_search").join("history.tsv"))
}

fn key(path: &str) -> String {
    path.to_lowercase()
}

impl History {
    pub fn load() -> Self {
        let text = path()
            .and_then(|p| fs::read_to_string(p).ok())
            .unwrap_or_default();
        Self::parse(&text, now())
    }

    fn parse(text: &str, now: u64) -> Self {
        let mut entries = HashMap::new();
        for line in text.lines() {
            let mut parts = line.splitn(3, '\t');
            let (Some(weight), Some(last), Some(path)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Ok(weight), Ok(last)) = (weight.parse::<f64>(), last.parse::<u64>()) else {
                continue;
            };
            let entry = Entry { weight, last };
            if weight.is_finite() && !path.is_empty() && entry.weight_at(now) >= FORGET_BELOW {
                entries.insert(key(path), entry);
            }
        }
        Self { entries }
    }

    fn render(&self) -> String {
        let mut lines: Vec<String> = self
            .entries
            .iter()
            .map(|(path, e)| format!("{:.4}\t{}\t{}\n", e.weight, e.last, path))
            .collect();
        lines.sort();
        lines.concat()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA"))?;
        fs::create_dir_all(path.parent().expect("history filename has a parent"))?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, self.render())?;
        fs::rename(tmp, path)
    }

    pub fn clear(&mut self) -> io::Result<()> {
        self.entries.clear();
        match path().map(fs::remove_file) {
            Some(Err(err)) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn record(&mut self, path: &str, now: u64) {
        let entry = self.entries.entry(key(path)).or_insert(Entry {
            weight: 0.0,
            last: now,
        });
        entry.weight = entry.weight_at(now) + 1.0;
        entry.last = now;
        if self.entries.len() > MAX_ENTRIES {
            self.trim(now);
        }
    }

    fn trim(&mut self, now: u64) {
        let mut weights: Vec<(f64, String)> = self
            .entries
            .iter()
            .map(|(path, e)| (e.weight_at(now), path.clone()))
            .collect();
        weights.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, path) in weights.into_iter().skip(KEEP_ENTRIES) {
            self.entries.remove(&path);
        }
    }

    /// Extra score for `path`: 0 when it was never opened, growing slowly with how
    /// often and how recently it was, and never above `MAX_BOOST`.
    pub fn boost(&self, path: &str, now: u64) -> i32 {
        self.entries.get(&key(path)).map_or(0, |e| {
            ((8.0 * e.weight_at(now).ln_1p()) as i32).clamp(0, MAX_BOOST)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    #[test]
    fn unopened_paths_get_no_boost() {
        assert_eq!(History::default().boost(r"C:\a.txt", 100), 0);
    }

    #[test]
    fn boost_grows_with_opens_and_is_capped() {
        let mut history = History::default();
        history.record(r"C:\a.txt", 1000);
        let once = history.boost(r"C:\a.txt", 1000);
        for _ in 0..5 {
            history.record(r"C:\a.txt", 1000);
        }
        let six = history.boost(r"C:\a.txt", 1000);
        assert!(once > 0 && six > once);
        for _ in 0..100_000 {
            history.record(r"C:\a.txt", 1000);
        }
        assert_eq!(history.boost(r"C:\a.txt", 1000), MAX_BOOST);
    }

    #[test]
    fn boost_fades_and_paths_ignore_case() {
        let mut history = History::default();
        history.record(r"C:\Docs\A.txt", 0);
        let fresh = history.boost(r"c:\docs\a.TXT", 0);
        let old = history.boost(r"C:\Docs\A.txt", 60 * DAY);
        assert!(fresh > 0 && old < fresh);
    }

    #[test]
    fn saved_text_round_trips_and_drops_junk_and_stale_entries() {
        let mut history = History::default();
        history.record(r"C:\a.txt", 5 * DAY);
        history.record(r"C:\b.txt", 5 * DAY);
        let text = format!(
            "{}garbage\nnan\t5\tC:\\c.txt\n1\t0\tC:\\old.txt\n",
            history.render()
        );
        let loaded = History::parse(&text, 5 * DAY + DAY * 365);
        assert!(loaded.is_empty());
        let loaded = History::parse(&text, 5 * DAY);
        assert_eq!(
            loaded.boost(r"C:\a.txt", 5 * DAY),
            history.boost(r"C:\a.txt", 5 * DAY)
        );
        assert_eq!(loaded.boost(r"C:\c.txt", 5 * DAY), 0);
        assert_eq!(loaded.entries.len(), 3);
    }

    #[test]
    fn list_is_capped_keeping_the_strongest() {
        let mut history = History::default();
        for _ in 0..10 {
            history.record(r"C:\favorite.txt", 10);
        }
        for i in 0..=MAX_ENTRIES {
            history.record(&format!(r"C:\file{i}.txt"), 10);
        }
        assert!(history.entries.len() <= MAX_ENTRIES);
        assert!(history.boost(r"C:\favorite.txt", 10) > 0);
    }
}
