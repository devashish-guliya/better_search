//! `bs report <index.bin>`: where the entries of a saved index are, by location class.
//! Only counts and folder names are printed, never file contents.

use std::collections::HashMap;
use std::path::Path;

use bs_engine::fmt::count as fmt_count;
use bs_index::{Index, Location};

const CLASSES: [(Location, &str); 5] = [
    (Location::Normal, "normal"),
    (
        Location::UserContent,
        "your files (Desktop, Documents, ...)",
    ),
    (Location::StartMenu, "Start Menu"),
    (Location::AppFiles, "program files"),
    (Location::Noisy, "system and app data"),
];

pub fn run(args: &[String]) -> Result<(), String> {
    let [path] = args else {
        return Err("usage: bs report <path to index.bin>".into());
    };
    let index = Index::load(Path::new(path)).map_err(|e| format!("cannot load {path}: {e}"))?;
    print!("{}", build(&index, 25));
    Ok(())
}

fn build(index: &Index, top: usize) -> String {
    let mut out = String::new();
    let mut per_class = [0usize; 5];
    let mut folders_per_class = [0usize; 5];
    let mut by_depth3: [HashMap<String, usize>; 2] = Default::default();
    let mut by_depth4: [HashMap<String, usize>; 2] = Default::default();
    let mut extensions: [HashMap<String, usize>; 2] = Default::default();
    let mut live = 0usize;
    for entry in 0..index.len() as u32 {
        if index.is_deleted(entry) || index.parent(entry).is_none() {
            continue;
        }
        live += 1;
        let location = index.location(entry);
        let class = CLASSES
            .iter()
            .position(|&(l, _)| l == location)
            .unwrap_or(0);
        per_class[class] += 1;
        if index.is_dir(entry) {
            folders_per_class[class] += 1;
        }
        let group = match location {
            Location::Noisy => 0,
            Location::AppFiles => 1,
            _ => continue,
        };
        let mut chain = ancestors(index, entry);
        // A file is counted in its folder, not as a folder of its own.
        if !index.is_dir(entry) {
            chain.pop();
        }
        *by_depth3[group]
            .entry(chain[..chain.len().min(4)].join("\\"))
            .or_default() += 1;
        *by_depth4[group]
            .entry(chain[..chain.len().min(5)].join("\\"))
            .or_default() += 1;
        if !index.is_dir(entry) {
            let name = index.name(entry).to_lowercase();
            let ext = name.rsplit_once('.').map_or("(none)", |(_, e)| e);
            *extensions[group].entry(ext.to_owned()).or_default() += 1;
        }
    }
    out.push_str(&format!("{} entries\n\n", fmt_count(live)));
    out.push_str("By class (entries, of which folders):\n");
    for (i, (_, label)) in CLASSES.iter().enumerate() {
        let share = 100.0 * per_class[i] as f64 / live.max(1) as f64;
        out.push_str(&format!(
            "  {:<40} {:>10}  {:>5.1}%  folders {:>9}\n",
            label,
            fmt_count(per_class[i]),
            share,
            fmt_count(folders_per_class[i])
        ));
    }
    for (group, title) in [(0, "system and app data"), (1, "program files")] {
        section(
            &mut out,
            &format!("{title}: biggest folders, 3 levels deep"),
            &by_depth3[group],
            top,
        );
        section(
            &mut out,
            &format!("{title}: biggest folders, 4 levels deep"),
            &by_depth4[group],
            top,
        );
        section(
            &mut out,
            &format!("{title}: most common file types"),
            &extensions[group],
            top,
        );
    }
    out
}

/// Folder names from the drive down to `entry`, e.g. `["C:", "Users", "hp"]`.
fn ancestors(index: &Index, entry: u32) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = Some(entry);
    while let Some(e) = current {
        names.push(index.name(e));
        current = index.parent(e);
        if names.len() > 512 {
            break;
        }
    }
    names.reverse();
    names
}

fn section(out: &mut String, title: &str, counts: &HashMap<String, usize>, top: usize) {
    let mut rows: Vec<(&String, &usize)> = counts.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    out.push_str(&format!("\n{title}:\n"));
    for (name, count) in rows.into_iter().take(top) {
        out.push_str(&format!("  {:>10}  {}\n", fmt_count(*count), name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bs_index::IndexBuilder;

    #[test]
    fn counts_classes_and_groups_by_folder() {
        let mut b = IndexBuilder::new();
        b.begin_volume("C:", 0);
        b.push(1, 0, "Windows", true, false);
        b.push(2, 1, "a.dll", false, false);
        b.push(3, 1, "b.dll", false, false);
        b.push(4, 0, "Docs", true, false);
        b.push(5, 4, "x.txt", false, false);
        b.end_volume();
        let report = build(&b.finish(), 5);
        assert!(report.starts_with("5 entries"));
        assert!(report.contains("3  C:\\Windows") || report.contains("3  C:\\Windows\n"));
        assert!(report.contains("2  dll"));
    }
}
