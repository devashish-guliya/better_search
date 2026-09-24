//! Generates a realistic-looking fake file tree for benchmarking at any size.

use bs_index::IndexBuilder;

const WORDS: &[&str] = &[
    "report",
    "invoice",
    "photo",
    "image",
    "backup",
    "project",
    "notes",
    "budget",
    "config",
    "data",
    "final",
    "draft",
    "resume",
    "letter",
    "summary",
    "presentation",
    "music",
    "video",
    "screenshot",
    "document",
    "setup",
    "install",
    "readme",
    "license",
    "main",
    "index",
    "test",
    "build",
    "release",
    "debug",
    "client",
    "server",
    "user",
    "profile",
    "settings",
    "cache",
    "temp",
    "log",
    "archive",
    "holiday",
    "family",
    "wedding",
    "trip",
    "school",
    "work",
    "tax",
    "contract",
    "meeting",
    "plan",
    "design",
    "logo",
    "icon",
    "banner",
    "theme",
    "style",
    "script",
    "module",
    "package",
    "library",
    "game",
    "save",
    "level",
    "map",
    "texture",
    "sound",
    "font",
    "chart",
    "table",
    "export",
    "import",
    "home",
    "office",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "old",
    "new",
    "copy",
    "version",
    "update",
];

const EXTENSIONS: &[&str] = &[
    "txt", "pdf", "docx", "xlsx", "pptx", "jpg", "png", "mp4", "mp3", "zip", "exe", "dll", "js",
    "json", "html", "css", "rs", "py", "cs", "log", "tmp", "ini", "xml", "lnk", "md",
];

const COMMON_NAMES: &[&str] = &[
    "index.js",
    "package.json",
    "README.md",
    "LICENSE",
    "__init__.py",
    "icon.png",
    "main.rs",
    "style.css",
    "desktop.ini",
    "Thumbs.db",
    "index.d.ts",
    "CHANGELOG.md",
];

/// Small deterministic generator so every run produces the same tree.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

pub fn generate(builder: &mut IndexBuilder, count: usize) {
    builder.begin_volume("S:", 0);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut dirs: Vec<u64> = vec![0];
    let mut name = String::with_capacity(64);
    for id in 1..=count as u64 {
        // Favor recently created folders so the tree gets depth instead of one flat level.
        let parent = if rng.below(4) == 0 {
            dirs[rng.below(dirs.len())]
        } else {
            dirs[dirs.len() - 1 - rng.below(dirs.len().min(64))]
        };
        let is_dir = rng.below(8) == 0;
        make_name(&mut rng, is_dir, &mut name);
        builder.push(id, parent, &name, is_dir, rng.below(50) == 0);
        if is_dir {
            dirs.push(id);
        }
    }
    builder.end_volume();
}

fn make_name(rng: &mut Rng, is_dir: bool, out: &mut String) {
    out.clear();
    if !is_dir && rng.below(5) == 0 {
        out.push_str(rng.pick(COMMON_NAMES));
        return;
    }
    let words = 1 + rng.below(3);
    let separator = rng.pick(&["_", "-", " ", ""]);
    for i in 0..words {
        let word = rng.pick(WORDS);
        if i > 0 {
            out.push_str(separator);
        }
        if separator.is_empty() && i > 0 {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        } else {
            out.push_str(word);
        }
    }
    if rng.below(3) == 0 {
        out.push_str(&format!("{}", 1990 + rng.below(40)));
    }
    if !is_dir {
        out.push('.');
        out.push_str(rng.pick(EXTENSIONS));
    }
}
