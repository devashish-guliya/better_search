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

# Also index the contents of clutter folders (skipped by default, see below).
.\target\release\bs.exe --all

# No admin needed: walk a folder, or generate fake data.
.\target\release\bs.exe --walk $env:USERPROFILE
.\target\release\bs.exe --synthetic 5000000 --bench
```

In the interactive prompt, type a query to search. Commands:

| Command | Effect |
|---|---|
| `:changes` | Show the latest file changes picked up from the journal |
| `:stats` | Show entry count and memory use |
| `:save` | Write the snapshot now |

Searches always use the live in-memory index. The snapshot on disk only makes the next
start fast, so it is written rarely: once an hour if something changed, and on a normal
quit. Nothing is written when Windows shuts down; changes made since the last save are
replayed from the NTFS change journal on the next start.

Deleted and renamed files leave some unused space in the index. Once that reaches about
2% of the index, it is rebuilt into a clean copy in the background during the next save.
Searches keep running on the old copy until the new one is swapped in.

The snapshot is stored at `%LOCALAPPDATA%\better_search\index.bin`. It is thrown away and
the drives are read again if a drive's serial number or journal changed, if too many
changes were missed while the tool was closed, or if the clutter setting changed.

Drives without a change journal get one (32 MB, the size Windows uses for the system
drive), so their snapshot stays valid too.

### Clutter folders

By default the contents of folders no ordinary user searches are left out: `node_modules`,
`.git`, Python and Rust package folders, and inside system or app-data areas also caches,
temp folders, logs and Windows component stores such as `WinSxS`. The folders themselves
stay searchable. New files are only added when their folder is in the index, so files
created inside skipped folders stay out, and a folder moved into a skipped folder leaves
the index together with its contents. The full list is in
`crates/index/src/lib.rs` (`SKIP_ANYWHERE`, `SKIP_IN_NOISY`).

After 60 seconds without a search the index is allowed to leave RAM; the first search after
that reads it back in.

## Checks

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
