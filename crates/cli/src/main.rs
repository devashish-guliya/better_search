//! Command-line prototype: build or load the index, keep it live, print stats, search.

mod live;
mod memory;
mod synthetic;
mod walk;

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bs_index::{Index, IndexBuilder};
use bs_query::{Query, Session, search};

use live::Shared;

const USAGE: &str = "\
Usage:
  bs [DRIVE...]          Index fixed NTFS drives (all if none given) and keep the index
                         live while running. Needs admin. The index is saved to
                         %LOCALAPPDATA%\\better_search\\index.bin so the next start is instant.
  bs --walk <FOLDER>     Index a folder by walking it. No admin needed.
  bs --synthetic <N>     Index N generated fake entries, for benchmarking.

Options:
  --bench                Run a fixed set of timed queries and exit.
  --rescan               Ignore the saved index and scan the drives again.
  -n <COUNT>             Number of results to show (default 20).
  -h, --help             Show this help.

While searching:
  :changes               Show the latest file changes picked up live.
  :stats                 Show index size and memory use.
  :save                  Save the index now.
  :q                     Quit (the index is saved first).

Examples:
  bs                     bs C D          bs --walk %USERPROFILE%
  bs --synthetic 5000000 --bench";

enum Source {
    Ntfs(Vec<char>),
    Walk(PathBuf),
    Synthetic(usize),
}

struct Args {
    source: Source,
    bench: bool,
    rescan: bool,
    limit: usize,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut drives = Vec::new();
    let mut source = None;
    let mut bench = false;
    let mut rescan = false;
    let mut limit = 20;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--bench" => bench = true,
            "--rescan" => rescan = true,
            "-n" => {
                let value = args.next().ok_or("-n needs a number")?;
                limit = value
                    .parse()
                    .map_err(|_| format!("invalid count: {value}"))?;
            }
            "--walk" => {
                let folder = args.next().ok_or("--walk needs a folder")?;
                source = Some(Source::Walk(PathBuf::from(folder)));
            }
            "--synthetic" => {
                let value = args.next().ok_or("--synthetic needs a count")?;
                let count = value
                    .replace(['_', ','], "")
                    .parse()
                    .map_err(|_| format!("invalid count: {value}"))?;
                source = Some(Source::Synthetic(count));
            }
            other => {
                let letter = other.trim_end_matches(['\\', ':']);
                match letter.chars().collect::<Vec<_>>().as_slice() {
                    [c] if c.is_ascii_alphabetic() => drives.push(c.to_ascii_uppercase()),
                    _ => return Err(format!("unknown argument: {other}")),
                }
            }
        }
    }
    let source = match source {
        Some(_) if !drives.is_empty() => {
            return Err("drive letters cannot be combined with --walk or --synthetic".into());
        }
        Some(s) => s,
        None => Source::Ntfs(drives),
    };
    Ok(Some(Args {
        source,
        bench,
        rescan,
        limit,
    }))
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let started = Instant::now();
    let live = matches!(args.source, Source::Ntfs(_));
    let (index, saved) = match build_index(&args.source, args.rescan) {
        Ok(result) => result,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    };
    print_stats(&index, Some(started.elapsed()));

    let shared = Arc::new(Shared::new(index));
    if saved {
        live::mark_saved(&shared);
    }
    if args.bench {
        run_bench(&shared.index.read().unwrap(), args.limit);
    } else {
        if live {
            live::start_background(&shared);
            println!("Live updates are on: new, renamed and deleted files show up right away.");
        }
        interactive(&shared, args.limit, live);
    }
    if live {
        shared.stop();
        live::save_shared(&shared);
    }
    ExitCode::SUCCESS
}

fn build_index(source: &Source, rescan: bool) -> Result<(Index, bool), String> {
    let mut builder = IndexBuilder::new();
    match source {
        Source::Ntfs(requested) => {
            let letters = if requested.is_empty() {
                bs_ntfs::fixed_ntfs_volumes()
            } else {
                requested.clone()
            };
            if letters.is_empty() {
                return Err("no fixed NTFS drives found".into());
            }
            return live::load_or_scan(&letters, rescan);
        }
        Source::Walk(folder) => {
            let t = Instant::now();
            let count = walk::walk(&mut builder, folder)
                .map_err(|e| format!("cannot walk {}: {e}", folder.display()))?;
            println!(
                "{}  {} entries in {}",
                folder.display(),
                fmt_count(count),
                fmt_duration(t.elapsed())
            );
        }
        Source::Synthetic(count) => {
            let t = Instant::now();
            synthetic::generate(&mut builder, *count);
            println!(
                "S:  {} generated entries in {}",
                fmt_count(*count),
                fmt_duration(t.elapsed())
            );
        }
    }
    Ok((builder.finish(), false))
}

fn print_stats(index: &Index, elapsed: Option<Duration>) {
    let entries = index.live_len();
    let usage = index.memory_usage();
    let per_entry = |bytes: usize| bytes as f64 / entries.max(1) as f64;
    println!();
    match elapsed {
        Some(t) => println!(
            "Ready: {} files and folders in {}",
            fmt_count(entries),
            fmt_duration(t)
        ),
        None => println!("{} files and folders", fmt_count(entries)),
    }
    if index.deleted_len() > 0 {
        println!(
            "  deleted, awaiting cleanup: {}",
            fmt_count(index.deleted_len())
        );
    }
    println!(
        "  unique names:    {} ({:.0}% of entries)",
        fmt_count(index.names().len()),
        100.0 * index.names().len() as f64 / entries.max(1) as f64
    );
    println!(
        "  index memory:    {} total, {:.1} bytes per entry",
        fmt_bytes(usage.total()),
        per_entry(usage.total())
    );
    println!(
        "    names {} · entries {} · change lookup {} · name lookup {}",
        fmt_bytes(usage.name_bytes),
        fmt_bytes(usage.entry_bytes),
        fmt_bytes(usage.lookup_bytes),
        fmt_bytes(usage.interner_bytes)
    );
    if let Some(mem) = memory::process_memory() {
        println!(
            "  process memory:  private {} · working set {} · peak working set {} (peak includes indexing)",
            fmt_bytes(mem.private),
            fmt_bytes(mem.working_set),
            fmt_bytes(mem.peak_working_set)
        );
    }
    println!();
}

fn interactive(shared: &Shared, limit: usize, live: bool) {
    println!("Type to search. Separate words to require all of them. Empty line or :q to quit.");
    let stdin = io::stdin();
    let mut session = Session::new();
    let mut line = String::new();
    loop {
        print!("search> ");
        let _ = io::stdout().flush();
        line.clear();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        // PowerShell prefixes piped input with a UTF-8 byte order mark.
        let input = line.trim_start_matches('\u{feff}').trim();
        match input {
            "" | ":q" | ":quit" => break,
            ":stats" => {
                print_stats(&shared.index.read().unwrap(), None);
                continue;
            }
            ":changes" => {
                let changes = shared.recent_changes();
                if changes.is_empty() {
                    println!("     no changes yet");
                }
                for change in changes {
                    println!("     {change}");
                }
                continue;
            }
            ":save" if live => {
                live::save_shared(shared);
                continue;
            }
            _ => {}
        }
        let Some(query) = Query::parse(input) else {
            continue;
        };

        let index = shared.index.read().unwrap();
        let t = Instant::now();
        let result = session.search(&index, &query, limit);
        let search_time = t.elapsed();

        let t = Instant::now();
        let rows: Vec<(i32, String)> = result
            .hits
            .iter()
            .map(|hit| {
                let mut path = index.full_path(hit.entry);
                if index.is_dir(hit.entry) {
                    path.push('\\');
                }
                (hit.score, path)
            })
            .collect();
        let path_time = t.elapsed();
        drop(index);

        for (i, (score, path)) in rows.iter().enumerate() {
            println!("{:>3}. [{score:>4}] {path}", i + 1);
        }
        println!(
            "     {} matches · search {} · paths {}",
            fmt_count(result.total_matches),
            fmt_duration(search_time),
            fmt_duration(path_time)
        );
    }
}

fn median_time(runs: usize, mut f: impl FnMut()) -> (Duration, Duration, Duration) {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    (times[0], times[runs / 2], times[runs - 1])
}

fn run_bench(index: &Index, limit: usize) {
    const QUERIES: &[&str] = &[
        "e",
        "a",
        "exe",
        "png",
        "readme",
        "notepad",
        "config json",
        "photo 2024",
        "windows",
        "x9qz",
    ];
    const RUNS: usize = 15;
    println!("Fresh searches (as if pasted in one go):");
    println!(
        "{:<14} {:>12} {:>10} {:>10} {:>10}",
        "query", "matches", "min", "median", "max"
    );
    for &text in QUERIES {
        let query = Query::parse(text).expect("benchmark queries are not empty");
        let matches = search(index, &query, limit).total_matches;
        let (min, median, max) = median_time(RUNS, || {
            std::hint::black_box(search(index, &query, limit));
        });
        println!(
            "{:<14} {:>12} {:>10} {:>10} {:>10}",
            format!("\"{text}\""),
            fmt_count(matches),
            fmt_duration(min),
            fmt_duration(median),
            fmt_duration(max)
        );
    }

    // Each keystroke re-filters the previous results, as the search box will.
    const TYPED: &[&str] = &["notepad", "readme", "invoice 2024", "config.json"];
    println!();
    println!("Typing letter by letter (median per keystroke):");
    for &word in TYPED {
        let prefixes: Vec<&str> = word
            .char_indices()
            .skip(1)
            .map(|(i, _)| &word[..i])
            .chain([word])
            .collect();
        let prefixes: Vec<&str> = prefixes
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect();
        let mut per_key = vec![Vec::with_capacity(RUNS); prefixes.len()];
        for _ in 0..RUNS {
            let mut session = Session::new();
            for (k, prefix) in prefixes.iter().enumerate() {
                let query = Query::parse(prefix).expect("prefixes are not empty");
                let t = Instant::now();
                std::hint::black_box(session.search(index, &query, limit));
                per_key[k].push(t.elapsed());
            }
        }
        let cells: Vec<String> = prefixes
            .iter()
            .zip(&mut per_key)
            .map(|(prefix, times)| {
                times.sort();
                format!("{prefix:?} {}", fmt_duration(times[RUNS / 2]))
            })
            .collect();
        println!("  {}", cells.join(" · "));
    }
}

pub(crate) fn fmt_count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(crate) fn fmt_bytes(bytes: usize) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MB", bytes as f64 / MB)
}

pub(crate) fn fmt_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        format!("{ms:.1} ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_drive_letters() {
        let args = parse(&["c", "D:", "e:\\"]).unwrap().unwrap();
        assert!(matches!(args.source, Source::Ntfs(ref d) if d == &['C', 'D', 'E']));
    }

    #[test]
    fn parses_options() {
        let args = parse(&["--synthetic", "1_000", "--bench", "--rescan", "-n", "5"])
            .unwrap()
            .unwrap();
        assert!(matches!(args.source, Source::Synthetic(1000)));
        assert!(args.bench);
        assert!(args.rescan);
        assert_eq!(args.limit, 5);
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse(&["--walk"]).is_err());
        assert!(parse(&["hello"]).is_err());
        assert!(parse(&["C", "--synthetic", "10"]).is_err());
    }

    #[test]
    fn formats_counts() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1234567), "1,234,567");
    }
}
