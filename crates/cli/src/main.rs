//! Phase 1 prototype: build the index, print memory and timing stats, then search.

mod memory;
mod synthetic;
mod walk;

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use bs_index::{Index, IndexBuilder};
use bs_query::{Query, search};

const USAGE: &str = "\
Usage:
  bs [DRIVE...]          Index fixed NTFS drives (all if none given). Needs admin.
  bs --walk <FOLDER>     Index a folder by walking it. No admin needed.
  bs --synthetic <N>     Index N generated fake entries, for benchmarking.

Options:
  --bench                Run a fixed set of timed queries and exit.
  -n <COUNT>             Number of results to show (default 20).
  -h, --help             Show this help.

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
    limit: usize,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut drives = Vec::new();
    let mut source = None;
    let mut bench = false;
    let mut limit = 20;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--bench" => bench = true,
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
    let index = match build_index(&args.source) {
        Ok(index) => index,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    };
    print_stats(&index, started.elapsed());

    if args.bench {
        run_bench(&index, args.limit);
    } else {
        interactive(&index, args.limit);
    }
    ExitCode::SUCCESS
}

fn build_index(source: &Source) -> Result<Index, String> {
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
            for letter in letters {
                index_ntfs_volume(&mut builder, letter)?;
            }
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
    let t = Instant::now();
    let index = builder.finish();
    println!("Finalized index in {}", fmt_duration(t.elapsed()));
    Ok(index)
}

fn index_ntfs_volume(builder: &mut IndexBuilder, letter: char) -> Result<(), String> {
    let t = Instant::now();
    let volume = bs_ntfs::Volume::open(letter).map_err(|e| {
        if e.kind() == io::ErrorKind::PermissionDenied {
            format!(
                "cannot open drive {letter}: without administrator rights.\n\
                 Open a terminal with \"Run as administrator\" and try again,\n\
                 or use --walk <FOLDER> to test without admin."
            )
        } else {
            format!("cannot open drive {letter}: {e}")
        }
    })?;
    let journal = volume.journal();

    builder.begin_volume(&format!("{letter}:"), bs_ntfs::ROOT_RECORD);
    let mut count = 0usize;
    let result = volume.enumerate(|record| {
        count += 1;
        builder.push_utf16(
            record.record_number(),
            record.parent_record_number(),
            record.name,
            record.is_dir(),
            record.is_hidden_or_system(),
        );
    });
    builder.end_volume();
    result.map_err(|e| format!("reading drive {letter}: failed: {e}"))?;

    let journal = match journal {
        Some(j) => format!("journal id {:#x}, next USN {}", j.journal_id, j.next_usn),
        None => "change journal not active".to_owned(),
    };
    println!(
        "{letter}:  {} entries in {}  ({journal})",
        fmt_count(count),
        fmt_duration(t.elapsed())
    );
    Ok(())
}

fn print_stats(index: &Index, elapsed: Duration) {
    let entries = index.len();
    let usage = index.memory_usage();
    let per_entry = |bytes: usize| bytes as f64 / entries.max(1) as f64;
    println!();
    println!(
        "Indexed {} files and folders in {}",
        fmt_count(entries),
        fmt_duration(elapsed)
    );
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
        "    names {} · entries {} · change lookup {}",
        fmt_bytes(usage.name_bytes),
        fmt_bytes(usage.entry_bytes),
        fmt_bytes(usage.lookup_bytes)
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

fn interactive(index: &Index, limit: usize) {
    println!("Type to search. Separate words to require all of them. Empty line or :q to quit.");
    let stdin = io::stdin();
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
        if input.is_empty() || input == ":q" || input == ":quit" {
            break;
        }
        let Some(query) = Query::parse(input) else {
            continue;
        };

        let t = Instant::now();
        let result = search(index, &query, limit);
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
    println!(
        "{:<14} {:>12} {:>10} {:>10} {:>10}",
        "query", "matches", "min", "median", "max"
    );
    for &text in QUERIES {
        let query = Query::parse(text).expect("benchmark queries are not empty");
        let matches = search(index, &query, limit).total_matches;
        let mut times: Vec<Duration> = (0..RUNS)
            .map(|_| {
                let t = Instant::now();
                std::hint::black_box(search(index, &query, limit));
                t.elapsed()
            })
            .collect();
        times.sort();
        println!(
            "{:<14} {:>12} {:>10} {:>10} {:>10}",
            format!("\"{text}\""),
            fmt_count(matches),
            fmt_duration(times[0]),
            fmt_duration(times[RUNS / 2]),
            fmt_duration(times[RUNS - 1])
        );
    }
}

fn fmt_count(n: usize) -> String {
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

fn fmt_bytes(bytes: usize) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MB", bytes as f64 / MB)
}

fn fmt_duration(d: Duration) -> String {
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
        let args = parse(&["--synthetic", "1_000", "--bench", "-n", "5"])
            .unwrap()
            .unwrap();
        assert!(matches!(args.source, Source::Synthetic(1000)));
        assert!(args.bench);
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
