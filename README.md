# better_search

Very fast file and folder name search for Windows.

Phase 1 is a command-line prototype that reads the file list of NTFS drives directly,
builds a compact in-memory index and searches it.

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

# No admin needed: walk a folder, or generate fake data.
.\target\release\bs.exe --walk $env:USERPROFILE
.\target\release\bs.exe --synthetic 5000000 --bench
```

## Checks

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
