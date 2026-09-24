# better_search

Very fast file and folder name search for Windows.

The current prototype is a command-line tool. It reads the file list of NTFS drives directly,
builds a compact in-memory index and searches it. It then follows the NTFS change journal
so the index stays current, and saves a compressed snapshot so the next start is instant.

## Layout

| Crate | Purpose |
|---|---|
| `crates/ntfs` | Reads every file record of an NTFS volume with `FSCTL_ENUM_USN_DATA` |
| `crates/index` | Compact index: shared UTF-8 name table, parent links, location tags |
| `crates/query` | Parallel ranked search that keeps only the best results |
| `crates/cli` | `bs` prototype: builds the index, prints stats, interactive search and benchmarks |

## Usage

```powershell
cargo build --release

# Index all fixed NTFS drives. Needs a terminal opened with "Run as administrator".
.\target\release\bs.exe
.\target\release\bs.exe C D --bench

# Ignore the saved snapshot and read the drives again.
.\target\release\bs.exe --rescan

# No admin needed: walk a folder, or generate fake data.
.\target\release\bs.exe --walk $env:USERPROFILE
.\target\release\bs.exe --synthetic 5000000 --bench
```

In the interactive prompt, type a query to search. Commands:

| Command | Effect |
|---|---|
| `:changes` | Show the latest file changes picked up from the journal |
| `:stats` | Show entry count and memory use |
| `:save` | Write the snapshot now (it is also saved every 5 minutes and on exit) |

The snapshot is stored at `%LOCALAPPDATA%\better_search\index.bin`. It is thrown away and
the drives are read again if a drive's serial number or journal changed, or if too many
changes were missed while the tool was closed.

## Checks

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
