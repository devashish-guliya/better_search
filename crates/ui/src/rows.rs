//! What the result list shows besides the hits themselves: section headings that
//! group hits by kind, and which letters of a name matched the search.

use bs_pipe::Hit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Apps,
    Settings,
    Folders,
    Documents,
    Media,
    Files,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Apps => "Apps",
            Kind::Settings => "Settings",
            Kind::Folders => "Folders",
            Kind::Documents => "Documents",
            Kind::Media => "Photos, videos and music",
            Kind::Files => "Other files",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Section(Kind),
    Hit(usize),
}

/// Puts hits of the same kind together, sections in order of their best hit and hits
/// in their ranked order within a section. Headings appear only when there is more
/// than one kind; the hits come back reordered to match the rows.
pub fn group(hits: Vec<Hit>, kind_of: impl Fn(&Hit) -> Kind) -> (Vec<Hit>, Vec<Row>) {
    let kinds: Vec<Kind> = hits.iter().map(&kind_of).collect();
    let mut order: Vec<Kind> = Vec::new();
    for &kind in &kinds {
        if !order.contains(&kind) {
            order.push(kind);
        }
    }
    if order.len() < 2 {
        let rows = (0..hits.len()).map(Row::Hit).collect();
        return (hits, rows);
    }
    let mut slots: Vec<Option<Hit>> = hits.into_iter().map(Some).collect();
    let mut sorted = Vec::with_capacity(slots.len());
    let mut rows = Vec::with_capacity(slots.len() + order.len());
    for kind in order {
        rows.push(Row::Section(kind));
        for (slot, &k) in slots.iter_mut().zip(&kinds) {
            if k == kind
                && let Some(hit) = slot.take()
            {
                rows.push(Row::Hit(sorted.len()));
                sorted.push(hit);
            }
        }
    }
    (sorted, rows)
}

/// For each UTF-16 unit of `name`, whether it is part of what the search matched:
/// each term's best occurrence (at a word start if there is one), or the word
/// initials it abbreviates (`vsc` in `Visual Studio Code`).
pub fn highlight(name: &str, query: &str) -> Vec<bool> {
    let chars: Vec<char> = name.chars().collect();
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let lower: Vec<char> = chars.iter().map(|&c| fold(c)).collect();
    let word_start = |i: usize| {
        i == 0
            || !lower[i - 1].is_alphanumeric()
            || (chars[i].is_uppercase() && chars[i - 1].is_lowercase())
    };
    let mut marks = vec![false; chars.len()];
    for term in query.split_whitespace() {
        // `ext:pdf` filters; it does not match name letters.
        if term.contains(':') {
            continue;
        }
        let term: Vec<char> = term.chars().map(fold).collect();
        if term.len() > lower.len() {
            continue;
        }
        let starts: Vec<usize> = (0..=lower.len() - term.len())
            .filter(|&i| lower[i..i + term.len()] == term[..])
            .collect();
        if let Some(&at) = starts.iter().find(|&&i| word_start(i)).or(starts.first()) {
            marks[at..at + term.len()].fill(true);
            continue;
        }
        let mut picked = Vec::new();
        for i in (0..lower.len()).filter(|&i| lower[i].is_alphanumeric() && word_start(i)) {
            if picked.len() < term.len() && lower[i] == term[picked.len()] {
                picked.push(i);
            }
        }
        if picked.len() == term.len() {
            for i in picked {
                marks[i] = true;
            }
        }
    }
    chars
        .iter()
        .zip(marks)
        .flat_map(|(c, mark)| std::iter::repeat_n(mark, c.len_utf16()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, score: i32) -> Hit {
        Hit {
            path: path.into(),
            is_dir: false,
            score,
        }
    }

    #[test]
    fn hits_are_grouped_by_kind_in_order_of_their_best_hit() {
        let hits = vec![
            hit("a.lnk", 9),
            hit("b.pdf", 8),
            hit("c.lnk", 7),
            hit("d.pdf", 6),
        ];
        let kind = |h: &Hit| {
            if h.path.ends_with(".lnk") {
                Kind::Apps
            } else {
                Kind::Documents
            }
        };
        let (sorted, rows) = group(hits, kind);
        let paths: Vec<&str> = sorted.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["a.lnk", "c.lnk", "b.pdf", "d.pdf"]);
        assert_eq!(
            rows,
            [
                Row::Section(Kind::Apps),
                Row::Hit(0),
                Row::Hit(1),
                Row::Section(Kind::Documents),
                Row::Hit(2),
                Row::Hit(3),
            ]
        );
        let (_, rows) = group(vec![hit("a.pdf", 1), hit("b.pdf", 1)], |_| Kind::Documents);
        assert_eq!(rows, [Row::Hit(0), Row::Hit(1)]);
    }

    #[test]
    fn matched_letters_are_marked() {
        let marked = |name: &str, query: &str| -> String {
            name.chars()
                .zip(highlight(name, query))
                .map(|(c, m)| if m { c.to_ascii_uppercase() } else { c })
                .collect()
        };
        assert_eq!(marked("calculator", "calc"), "CALCulator");
        // A word start beats an earlier match inside a word.
        assert_eq!(
            marked("notes and stickynotes", "note"),
            "NOTEs and stickynotes"
        );
        assert_eq!(marked("sticky notes", "notes"), "sticky NOTES");
        assert_eq!(marked("visual studio code", "vsc"), "Visual Studio Code");
        assert_eq!(marked("myreportviewer.cs", "rep"), "myREPortviewer.cs");
        assert_eq!(marked("report.pdf", "ext:pdf rep"), "REPort.pdf");
        assert_eq!(marked("abc", "zzzz"), "abc");
        assert_eq!(highlight("a😀b", "b"), [false, false, false, true]);
    }
}
