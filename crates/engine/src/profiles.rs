//! Keeps each user's searches out of other users' profile folders
//! (`<drive>\Users\<name>`). The profile folders themselves stay visible, like the
//! user list in Explorer; only what is inside them is hidden.

use std::path::Path;

use bs_index::Index;

/// Not inside any profile folder.
const NONE: u8 = 0;
/// More profile folders than slots; hidden from everyone.
const OVERFLOW: u8 = 254;
const UNKNOWN: u8 = 255;
const MAX_DEPTH: usize = 512;
/// Profile folders every user may look into.
const SHARED: &[&str] = &["public"];

/// Which profile folder each entry's contents belong to, for one index generation.
pub struct Profiles {
    /// The index's users epoch (see `Index::users_epoch`). While it stays the same,
    /// ownership of existing entries cannot have moved between profiles, so this map
    /// stays exact and only needs extending with new entries.
    epoch: u64,
    generation: u64,
    /// Per entry: the slot its children belong to. A profile folder holds its own slot;
    /// everything else holds its parent's value. So an entry's owner is the value of
    /// its parent, and the profile folder itself is owned by nobody.
    inherit: Vec<u8>,
    /// Lowercased folder name of each slot, starting at slot 1.
    names: Vec<String>,
}

impl Profiles {
    pub fn build(index: &Index, epoch: u64) -> Self {
        let len = index.len();
        let mut inherit = vec![UNKNOWN; len];
        let is_root = |e: u32| index.parent(e).is_none();

        let mut users_folders = Vec::new();
        for e in 0..len as u32 {
            if is_root(e) {
                inherit[e as usize] = NONE;
            } else if index.is_dir(e)
                && !index.is_deleted(e)
                && index.parent(e).is_some_and(is_root)
                && index.name(e).eq_ignore_ascii_case("users")
            {
                users_folders.push(e);
                inherit[e as usize] = NONE;
            }
        }

        let mut names = Vec::new();
        for e in 0..len as u32 {
            let in_users = index.parent(e).is_some_and(|p| users_folders.contains(&p));
            if in_users && index.is_dir(e) && !index.is_deleted(e) {
                let slot = if names.len() < usize::from(OVERFLOW - 1) {
                    names.push(index.name(e).to_lowercase());
                    names.len() as u8
                } else {
                    OVERFLOW
                };
                inherit[e as usize] = slot;
            }
        }

        let mut built = Self {
            epoch,
            generation: index.generation(),
            inherit,
            names,
        };
        built.fill_unknown(index, 0);
        built
    }

    /// Gives every entry from `from` on that has no value yet the value of its nearest
    /// ancestor.
    fn fill_unknown(&mut self, index: &Index, from: usize) {
        let mut chain = Vec::with_capacity(64);
        for e in from..index.len() {
            if self.inherit[e] != UNKNOWN {
                continue;
            }
            chain.clear();
            let mut current = e as u32;
            let value = loop {
                chain.push(current);
                match index.parent(current) {
                    Some(p) if self.inherit[p as usize] != UNKNOWN => {
                        break self.inherit[p as usize];
                    }
                    Some(p) if chain.len() < MAX_DEPTH => current = p,
                    _ => break NONE,
                }
            };
            for &c in &chain {
                self.inherit[c as usize] = value;
            }
        }
    }

    /// The index's users epoch this was built for; rebuild from scratch when it
    /// changes.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The same map brought up to date with `index`, provided the epoch is unchanged:
    /// ownership of existing entries cannot have moved between profiles, so only
    /// entries added since this map was built need a value, which they inherit from
    /// their parent chain. A compaction renumbers entries and bumps the epoch, so this
    /// must only be called when the epochs match.
    pub fn extended(&self, index: &Index) -> Self {
        debug_assert!(index.len() >= self.inherit.len());
        let old_len = self.inherit.len();
        let mut fresh = Self {
            epoch: self.epoch,
            generation: index.generation(),
            inherit: self.inherit.clone(),
            names: self.names.clone(),
        };
        fresh.inherit.resize(index.len(), UNKNOWN);
        fresh.fill_unknown(index, old_len);
        fresh
    }

    pub fn heap_bytes(&self) -> usize {
        self.inherit.capacity()
    }

    /// What a user whose profile folder is `own_profile` (for example
    /// `C:\Users\anna`) may see. `None` sees only shared profile folders.
    pub fn visibility(&self, own_profile: Option<&Path>) -> Visibility {
        let own = own_profile.and_then(profile_folder_name);
        let mut allowed = [false; 256];
        allowed[usize::from(NONE)] = true;
        for (i, name) in self.names.iter().enumerate() {
            // The same folder name on another drive is usually the user's old profile.
            let visible = SHARED.contains(&name.as_str()) || own.as_deref() == Some(name);
            allowed[i + 1] = visible;
        }
        Visibility { allowed }
    }

    /// Whether `entry` may appear in the results of a user with `visibility`.
    pub fn allows(&self, visibility: &Visibility, index: &Index, entry: u32) -> bool {
        index.parent(entry).is_none_or(|p| {
            self.inherit
                .get(p as usize)
                .is_some_and(|&slot| visibility.allowed[usize::from(slot)])
        })
    }
}

pub struct Visibility {
    allowed: [bool; 256],
}

/// Whether `path` (from [`Index::full_path`], for example `C:\Users\anna\x.txt`) is a
/// Users folder of the volume `label` (`C:`) or lies inside one. Used to notice when a
/// change can affect profile ownership, so the map is only rebuilt then.
pub fn touches_users(label: &str, path: &str) -> bool {
    let Some(rest) = path.strip_prefix(label) else {
        return false;
    };
    let first = rest
        .strip_prefix('\\')
        .unwrap_or(rest)
        .split('\\')
        .next()
        .unwrap_or("");
    first.eq_ignore_ascii_case("users")
}

/// `anna` for `C:\Users\anna` or `C:\Users\anna\`; `None` for paths not directly in a
/// `Users` folder at a drive root.
fn profile_folder_name(path: &Path) -> Option<String> {
    let text = path.to_str()?.trim_end_matches('\\');
    let mut parts = text.split('\\');
    let drive = parts.next()?;
    let users = parts.next()?;
    let name = parts.next()?;
    let valid = drive.len() == 2
        && drive.ends_with(':')
        && users.eq_ignore_ascii_case("users")
        && !name.is_empty()
        && parts.next().is_none();
    valid.then(|| name.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bs_index::{Change, IndexBuilder};

    fn build(paths: &[&str]) -> Index {
        let mut b = IndexBuilder::new();
        b.begin_volume("C:", 5);
        let mut known = std::collections::HashMap::new();
        let mut next = 100u64;
        for path in paths {
            let parts: Vec<&str> = path.split('\\').collect();
            let mut parent = 5u64;
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

    fn visible(index: &Index, own: Option<&str>) -> Vec<String> {
        let profiles = Profiles::build(index, 0);
        let vis = profiles.visibility(own.map(Path::new));
        let mut out: Vec<String> = (0..index.len() as u32)
            .filter(|&e| !index.is_deleted(e) && index.parent(e).is_some())
            .filter(|&e| profiles.allows(&vis, index, e))
            .map(|e| index.full_path(e))
            .collect();
        out.sort();
        out
    }

    const TREE: &[&str] = &[
        "Users\\anna\\Documents\\tax.pdf",
        "Users\\Bob\\Desktop\\secret.txt",
        "Users\\Public\\Music\\song.mp3",
        "Users\\desktop.ini",
        "Data\\Users\\carl\\x.txt",
        "Program Files\\app.exe",
    ];

    #[test]
    fn hides_other_profiles_but_keeps_the_folders() {
        let index = build(TREE);
        let seen = visible(&index, Some(r"C:\Users\anna"));
        assert!(seen.contains(&r"C:\Users\anna\Documents\tax.pdf".to_string()));
        assert!(seen.contains(&r"C:\Users\Bob".to_string()));
        assert!(!seen.iter().any(|p| p.starts_with(r"C:\Users\Bob\")));
        assert!(seen.contains(&r"C:\Users\Public\Music\song.mp3".to_string()));
        assert!(seen.contains(&r"C:\Users\desktop.ini".to_string()));
        // Only a Users folder at the drive root holds profiles.
        assert!(seen.contains(&r"C:\Data\Users\carl\x.txt".to_string()));
        assert!(seen.contains(&r"C:\Program Files\app.exe".to_string()));
    }

    #[test]
    fn profile_names_match_without_case() {
        let index = build(TREE);
        let seen = visible(&index, Some(r"c:\users\BOB\"));
        assert!(seen.contains(&r"C:\Users\Bob\Desktop\secret.txt".to_string()));
        assert!(!seen.iter().any(|p| p.starts_with(r"C:\Users\anna\")));
    }

    #[test]
    fn unknown_callers_see_only_shared_folders() {
        let index = build(TREE);
        let seen = visible(&index, Some(r"C:\Windows\system32\config\systemprofile"));
        assert!(!seen.iter().any(|p| p.starts_with(r"C:\Users\anna\")));
        assert!(!seen.iter().any(|p| p.starts_with(r"C:\Users\Bob\")));
        assert!(seen.contains(&r"C:\Users\Public\Music\song.mp3".to_string()));
        assert_eq!(visible(&index, None), seen);
    }

    #[test]
    fn follows_live_moves_after_rebuild() {
        let mut index = build(TREE);
        let before = Profiles::build(&index, 0);
        // Move Bob's Desktop folder into Program Files.
        let desktop = index.volumes()[0].entry_for_record(105).unwrap();
        assert_eq!(index.name(desktop), "Desktop");
        let program_files = index.volumes()[0].entry_for_record(115).unwrap();
        assert_eq!(index.name(program_files), "Program Files");
        index.apply(
            0,
            Change::Upsert {
                record: 105,
                parent_record: 115,
                name: "Desktop",
                is_dir: true,
                hidden: false,
            },
        );
        index.end_batch();
        assert_ne!(before.generation(), index.generation());
        let seen = visible(&index, Some(r"C:\Users\anna"));
        assert!(seen.contains(&r"C:\Program Files\Desktop\secret.txt".to_string()));
    }

    #[test]
    fn reads_profile_folder_names() {
        let name = |p: &str| profile_folder_name(Path::new(p));
        assert_eq!(name(r"C:\Users\Anna"), Some("anna".into()));
        assert_eq!(name(r"D:\Users\anna\"), Some("anna".into()));
        assert_eq!(name(r"C:\Users\anna\Documents"), None);
        assert_eq!(name(r"C:\Windows\system32\config\systemprofile"), None);
        assert_eq!(name(r"C:\Users"), None);
    }
}
