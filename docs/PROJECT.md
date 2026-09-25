# better_search: project record

This document records what has been built, how it works, why each decision was made,
what is settled, and what comes next. It is the hand-off point for anyone (or any new
chat session) continuing the work. Keep it current when decisions change.

Last updated after commit `913618b` ("Detect clutter folders on any PC").

---

## 1. Goal

The fastest and leanest file and folder **name** search for Windows:

- Finds files and folders by name as you type, in a few milliseconds.
- Uses as little RAM, CPU and disk as possible. Idle CPU should be zero.
- Local only. Nothing leaves the PC.
- One-click install, and instant access from anywhere: a tray icon, the **Alt+Space**
  hotkey, and a hover zone at the right screen edge that slides the search panel in.
- Target users: ordinary people with 1–2 TB drives, not only developers.

Out of scope: searching file **contents**. Only names are searched.

## 2. Development machine and test data

- Windows 11 (build 26200), Ryzen 5 3500U (4 cores / 8 threads).
- Rust 1.98.1, MSVC toolchain, edition 2024. Git 2.47.1. Git user: Devashish Guliya.
- Drives: `C:` 238 GB SSD. `D:` and `E:` are two partitions of one 932 GB HDD.
- About 4 million files and folders in total across the three drives.
- The repository is at `D:\better_search`, branch `main`, local only (no remote).

## 3. Commit history

| Commit | What it added |
|---|---|
| `d0c31f2` | Phase 1: workspace, NTFS enumeration, compact index, parallel search, `bs` console tool |
| `16bf834` | Live change-journal updates, zstd snapshot, faster search (folded names, type-ahead narrowing) |
| `0ba9ffc` | Creates missing change journals; stops memory growth after live changes |
| `4b12a23` | Clutter skipping, smaller snapshot (delta coding), hourly save, idle memory trim |
| `aca0454` | Sorted record lookup, non-blocking compaction, name table restored, "unknown parent → ignore" rule |
| `913618b` | Clutter detection that works on any PC: tool-only names, project-confirmed names, self-labelled folders |

## 4. Current results on the development machine

Measured with an elevated run of `bs --bench` after `913618b`:

| Measure | Value |
|---|---|
| Files and folders scanned | 3,978,539 |
| Kept in the index (clutter skipped) | 608,982 (85% skipped) |
| First full scan | about 35 s (C: 12.8 s, D: 19.5 s, E: 1.5 s; the HDD is the limit) |
| Normal start (load snapshot + catch up) | 60 ms |
| Index memory | 18.4 MB (names 8.7, entries 5.2, change lookup 2.3, name lookup 2.2) |
| Process private memory | about 20 MB |
| Snapshot on disk | 4.5 MB, saved in about 0.2 s |
| Typical search | 0.7–1.3 ms |
| Single-letter search (worst case) | 5–7 ms |
| Idle CPU | 0 |

History of the main numbers, to show what each change bought:

| After commit | Index RAM | Snapshot | Start | Typical search |
|---|---|---|---|---|
| `0ba9ffc` (everything indexed) | 92 MB | – | 0.35 s | 4–6 ms |
| `4b12a23` (clutter skipped) | 35 MB | 7.1 MB | 0.15 s | 2–3 ms |
| `aca0454` (sorted lookup, name table back) | 23.5 MB | 6.4 MB | 87 ms | 1.2–1.9 ms |
| `913618b` (universal clutter rules) | 18.4 MB | 4.5 MB | 60 ms | 0.7–1.3 ms |

---

## 5. Architecture

A Cargo workspace with four crates:

| Crate | Package | Purpose |
|---|---|---|
| `crates/ntfs` | `bs-ntfs` | Reads NTFS volumes: full file list and the change journal |
| `crates/index` | `bs-index` | The compact in-memory index, clutter rules, snapshot format |
| `crates/query` | `bs-query` | Ranked, parallel, case-insensitive name search |
| `crates/cli` | `bs-cli` (binary `bs`) | Console prototype: load/scan, live updates, saving, interactive search, benchmarks |

Dependencies are deliberately few: `windows-sys` (raw Win32 bindings), `hashbrown`,
`zstd`, `memchr`, `rayon`. Release profile: `opt-level=3`, LTO, one codegen unit,
`panic=abort`, stripped. A `release-small` profile (`opt-level="z"`) exists for shipping.

### 5.1 Reading the drives (`bs-ntfs`)

- **Full list:** `FSCTL_ENUM_USN_DATA` on the volume handle (`\\.\C:`). It returns every
  file record (record number, parent record number, name, attributes) straight from the
  Master File Table. This is far faster than walking folders. It needs administrator
  rights. Buffer: 1 MB.
- **Changes:** `FSCTL_READ_USN_JOURNAL` reads the NTFS change journal (the USN journal)
  from a saved position. Reasons used: create, delete, rename old/new name, basic info
  change and hard link change. Buffer: 64 KB. The read waits for new records, so an idle
  watcher uses no CPU.
- **Journal info:** `FSCTL_QUERY_USN_JOURNAL` gives the journal ID and the valid USN range.
- **Missing journals:** `FSCTL_CREATE_USN_JOURNAL` creates one when a drive has none
  (maximum 32 MB, growth 8 MB, the same size Windows uses on the system drive). `D:` and
  `E:` had no journal, which forced a full rescan at every start. The user approved
  creating them automatically.
- `fixed_ntfs_volumes()` lists fixed NTFS drives. `volume_serial()` identifies a disk.
- The volume root is always record 5.

### 5.2 The index (`bs-index`)

**Layout: parallel arrays (structure of arrays).** One slot per entry:

- `name_ids: Vec<u32>`: which name this entry has.
- `parents: Vec<u32>`: parent entry (`NO_PARENT = u32::MAX` for volume roots).
- `flags: Vec<u8>`: `DIR`, `HIDDEN`, a 2-bit location class, `DELETED`, `SKIPPED`.

Full paths are **never stored**. They are rebuilt by walking parent links, and only for
results that are actually shown.

**Name table.** All unique names are stored **lowercased** back to back in one UTF-8
buffer (`folded`), with an offsets array and a bitmask (`upper`) recording which bytes
were uppercase. That lets search scan one contiguous buffer with SIMD while still showing
original case. Case variants (`Readme` and `README`) are different names.

**Interner.** A hash table (hashbrown `HashTable`) from name to name ID, so entries with
the same name (`index.js`, `package.json`, ...) share one stored copy. Only 47% of
entries have a unique name. The interner costs about 2.2 MB. It is rebuilt from the
name table on load rather than saved.

**Record lookup (`RecordMap`).** The change journal refers to files by NTFS record
number, so each volume needs "record number → entry":

- `sorted: Vec<u32>`: record numbers of the volume's entries in entry order. Entries are
  laid out by increasing record number, so lookup is a binary search, and
  `entry = first_entry + position`.
- `recent: HashMap<u32, u32>`: entries created live since the last compaction.
- This replaced a full hash map and saves memory (about 2.3 MB for the whole index).

**Locations.** Each entry gets a class used for ranking and for clutter rules:

- `UserContent`: under `<drive>\Users\<profile>\{Desktop, Documents, Downloads,
  Pictures, Videos, Music, OneDrive}`. Ranked higher.
- `Noisy`: under `Windows`, `ProgramData`, `AppData`, `$`-folders at the drive root,
  `Recovery`, `node_modules`, `.git` and similar. Ranked lower.
- `Normal`: everything else.

A child inherits its parent's class unless its own name changes it. The classes are
recomputed when folders move.

**Building.** `IndexBuilder` takes records per volume, interns names, links parents
using the record lookup, then `finish()` assigns locations. Records usually arrive in
increasing record order; if not, entries go to `recent` and a compaction sorts them.

**Live updates (`Index::apply`).** Each journal change is either an upsert (create,
rename, move, attribute change) or a delete:

- **New entries are only added if their parent folder is in the index** and is not a
  skipped folder. Windows always reports a folder before the files in it, so a real
  folder is never missed. This one rule also keeps everything inside skipped folders
  out, without re-checking names.
- An existing entry moved under a folder that is not indexed (for example into
  `node_modules`) is removed together with everything below it.
- Deleted entries are only flagged `DELETED`. Their slots are reclaimed by compaction.
- `end_batch()` runs after each group of changes: re-checks project rules
  (section 5.3), removes orphans (entries below deleted or skipped folders), fixes
  locations if folders moved, and bumps the `generation` counter so search caches reset.
- A move into one's own subfolder is rejected (the entry goes under the root instead),
  and parent walks are capped at depth 512, so corrupt data can never hang the program.

**Compaction.** `compacted(&self)` builds a clean copy with only **read** access: drops
deleted entries and unused names, puts entries back in record order, and merges
`recent` into `sorted`. It takes about 0.2 s on the real index. `needs_compaction()` is
true when `(deleted + renames + recent) × 50 ≥ max(live, 1000)`, which is about 2%
garbage.

### 5.3 Clutter skipping

About 85% of all files on a typical PC are ones nobody searches by name: package
folders, build output, caches, Windows component stores. Leaving them out cut RAM,
snapshot size, start time and search time several times over.

**Principle:** only the **contents** of a clutter folder are left out. The folder itself
stays searchable (you can still find `node_modules` or `WinSxS`). A skipped folder has
the `SKIPPED` flag; nothing below it is in the index.

**Rules** (all in `crates/index/src/lib.rs`), current version `SKIP_RULES_VERSION = 2`:

1. **Drive roots:** folders starting with `$` (like `$Recycle.Bin`) and
   `System Volume Information`.
2. **Tool-only names, skipped anywhere** (`SKIP_ANYWHERE`): version control (`.git`,
   `.svn`, `.hg`, `.bzr`); JavaScript (`node_modules`, `bower_components`,
   `jspm_packages`, `.npm`, `.yarn`, `.pnpm-store`, `.next`, `.nuxt`, `.svelte-kit`,
   `.angular`, `.parcel-cache`, `.turbo`, `.expo`); Python (`__pycache__`,
   `site-packages`, `.venv`, `.tox`, `.nox`, `.pytest_cache`, `.mypy_cache`,
   `.ruff_cache`, `.ipynb_checkpoints`, `.conda`); JVM and Android (`.gradle`, `.m2`,
   `.ivy2`, `.android`, `.cxx`, `.externalNativeBuild`); .NET (`.nuget`, `.vs`); Rust,
   Dart, Haskell (`.rustup`, `.cargo`, `.dart_tool`, `.pub-cache`, `.stack-work`); C/C++
   (`CMakeFiles`, `vcpkg_installed`); infrastructure (`.terraform`, `.serverless`);
   general (`.cache`).
3. **Common names, skipped only inside system or app-data areas** (`SKIP_IN_NOISY`,
   applied when the parent's location is `Noisy`): `WinSxS`, `servicing`,
   `SoftwareDistribution`, `Installer`, `assembly`, `Microsoft.NET`, `DriverStore`,
   `Prefetch`, `Logs`, `Temp`, `Tmp`, `WER`, crash dump folders, many cache folders
   (`Cache`, `Code Cache`, `GPUCache`, shader caches, ...), `Service Worker`,
   `IndexedDB`, `blob_storage`, `File System`, `Package Cache`. The same names elsewhere
   (for example `D:\Projects\logs`) are indexed.
4. **Common names confirmed by a project file next to them** (`SKIP_IN_PROJECT`):

   | Folder | Skipped only if its parent folder contains |
   |---|---|
   | `target` | `Cargo.toml` |
   | `bin`, `packages` | a `.csproj`, `.vbproj`, `.fsproj` or `.sln` file |
   | `obj` | a .NET project file, or a Unity project |
   | `build` | Gradle files, `CMakeLists.txt`, `package.json` or `pubspec.yaml` |
   | `dist`, `out` | `package.json` |
   | `vendor` | `composer.json` or `go.mod` |
   | `Pods` | `Podfile` |
   | `Library`, `Temp`, `Logs` | both `Assets` and `ProjectSettings` folders (Unity) |
   | `Intermediate`, `DerivedDataCache` | a `.uproject` file (Unreal) |

5. **Folders that label themselves:** a folder that directly contains `CACHEDIR.TAG`
   (the standard cache-folder label), `pyvenv.cfg` (any Python virtual environment,
   whatever it is called) or `CMakeCache.txt` (a CMake build folder).

**How the rules run.**

- On a full scan, `skip_clutter()` first collects "marker bits" per folder (which marker
  files it directly contains) in one pass over all entries, then walks every entry
  parent-first and applies the rules. It takes about 0.37 s for 4 million entries, and
  reports how many entries each rule removed.
- Live: a new or renamed folder is checked against rules 1–3 at once. Anything that can
  affect rules 4–5 (a new marker file, or a new folder with a project-rule name) queues
  its parent folder; `end_batch()` re-checks only those folders. This handles a cloned
  repository where `target` appears before `Cargo.toml`: when `Cargo.toml` arrives, the
  `target` folder is skipped and its contents are removed.
- `--all` turns skipping off (skip rules version 0).

**Why not other approaches** (discussed and rejected):

- *Reading `.gitignore` files:* needs file reads, and hides files people do search for
  (`.env`, local notes).
- *Guessing by file count:* "a folder with 50,000 files is clutter" would also hide a
  photo library.

**Known limits.**

- Removing a project file, or renaming a folder away from a clutter name, does not bring
  its contents back until the next full scan (`--rescan`), because they were never read.
- A folder moved from a skipped area into a normal one appears, but its existing
  contents stay missing until the next full scan.
- Changing the rules requires bumping `SKIP_RULES_VERSION`, which forces one rescan
  (about 35 s on the development machine).

### 5.4 Search (`bs-query`)

- **Query:** whitespace-separated terms; every term must appear in the name
  (case-insensitive substring match).
- **Pass 1, per unique name:** for ASCII queries, one SIMD substring scan (`memchr`)
  over the lowercased name buffer finds candidate names; only names containing the
  longest term are scored. Repeated names are scored once. Non-ASCII queries use full
  Unicode lowercasing.
- **Name score** (0 means no match), per term the best of: exact name > prefix > word
  start (after punctuation, camelCase boundary, letter/digit boundary) > substring.
  Extras: prefix followed only by an extension counts as an exact stem match; `.exe`,
  `.lnk`, `.url`, `.appref-ms` get a boost; `.dll`, `.mui`, `.tmp`, `.log`, `.etl`,
  `.cat`, `.manifest`, `.pf`, `.pyc` get a penalty; longer names lose a little.
- **Pass 2, per entry:** name score plus location (`UserContent` +25, `Noisy` −45),
  folders +3, hidden −20. Each thread keeps only the best `limit` hits in a small heap,
  so the full match list is never built, even for millions of matches. Ties go to the
  lower entry number, so results are deterministic.
- **Type-ahead narrowing (`Session`):** when the new query only adds characters to the
  previous one, only the names that matched before are re-checked. The cache resets when
  the index `generation` changes.
- **Zero-match early exit:** if no name matches, pass 2 is skipped.
- Parallelism: `rayon`.

### 5.5 Snapshot (`crates/index/src/snapshot.rs`)

- Path: `%LOCALAPPDATA%\better_search\index.bin`.
- Format **version 3**: 8-byte header (`BSIX` + version), then one zstd stream (level 3,
  with frame checksum) holding: skip rules version, name buffer, uppercase bitmask,
  name offsets (delta-coded), name IDs, parents, flags, and per volume: label, root,
  root record, sync point (volume serial, journal ID, next USN), `first_entry`, sorted
  record list (delta-coded) and `recent` pairs.
- Written to a `.tmp` file, synced, then renamed, so a crash mid-save never leaves a
  broken snapshot.
- Loading validates everything (lengths, bounds, strictly increasing record list, IDs in
  range), so a damaged file is rejected instead of causing a panic.
- **When the snapshot is thrown away and the drives are rescanned:** the skip rules
  version differs; the set of drives differs; a drive's serial number changed (different
  disk); its journal ID changed (journal recreated); the saved position is outside the
  journal's valid range (too many changes while the program was closed); or the file is
  damaged or from another format version.
- On load, changes since the saved position are replayed from each drive's journal
  ("catch up"), which takes a few milliseconds.

### 5.6 The console tool (`bs`, `crates/cli`)

```
bs                     Index all fixed NTFS drives (needs "Run as administrator")
bs C D                 Only these drives
bs --walk <FOLDER>     Walk a folder instead (no admin needed; no live updates)
bs --synthetic <N>     N generated fake entries, for benchmarking
--bench                Run a fixed set of timed queries and exit
--rescan               Ignore the snapshot and read the drives again
--all                  Do not skip clutter
```

Interactive prompt: type to search; `:changes` shows recent journal changes, `:stats`
shows memory use, `:save` writes the snapshot, an empty line or `:q` quits.

Runtime behaviour (`crates/cli/src/live.rs`):

- `Shared` holds the index in an `RwLock`. Searches take a read lock.
- One **watcher thread per drive** follows its journal and applies changes in batches
  under the write lock.
- A **maintenance mutex** is taken by writers and by the saver before the write lock.
  So while a long read-locked job runs (compaction or save), no writer is queued on the
  `RwLock`. A queued writer would make new searches wait.
- **Saver thread:** wakes every 15 s. Saves once an hour if anything changed, and on a
  normal quit. When saving, it first compacts if `needs_compaction()`: builds the clean
  copy under a read lock, swaps it in under a brief write lock, and frees the old copy
  outside the lock.
- **Idle trim:** after 60 s without a search, `EmptyWorkingSet` lets Windows page the
  index out of RAM; the first search after that reads it back.
- No save at shutdown or logoff (no console control handler). Missed changes are
  replayed from the journal at the next start.

---

## 6. Decisions and the reasons behind them

| Decision | Why |
|---|---|
| Rust with raw Win32 (`windows-sys`), native UI planned | Smallest memory and binary size, no runtime (chosen over C#/.NET and web UIs) |
| Read the MFT via `FSCTL_ENUM_USN_DATA` instead of walking folders | Seconds instead of minutes, and it sees every file |
| Admin rights are acceptable | Required for MFT and journal access; the service (Phase 3) means the user only approves once at install |
| Follow the USN journal instead of re-scanning or `ReadDirectoryChangesW` | Exact, cheap, zero idle CPU, and allows catching up on changes made while closed |
| Auto-create missing journals (32 MB) | Without a journal every start is a full rescan; the user approved |
| Parallel arrays, no stored paths | Minimum bytes per entry (about 32 bytes including names and lookups) |
| Lowercased name buffer + uppercase bitmask | One SIMD scan per search, original case still shown |
| Keep the name interner (about 2.2 MB) | The user preferred search quality and correctness over a few MB; names are shared immediately after live changes |
| Sorted record list plus small `recent` map ("option C") | Less memory than a full hash map, still fast lookups |
| Compaction at about 2% garbage, built next to the live index | Keeps memory tight without ever blocking searches (user's choice) |
| Skip clutter contents, keep the folders | 85% fewer entries; nothing is invisible, the folders still show |
| Universal clutter rules (tool names, project markers, self-labels) | Works on any PC with no setup; avoids hiding real user folders called `target`, `build`, `Library` |
| No `.gitignore` parsing, no file-count heuristics | Would hide files users want, or need file reads |
| Save hourly and on normal quit, not at shutdown or startup | The journal makes frequent saves unnecessary; avoids disk writes (user left this to the assistant) |
| zstd level 3 with delta coding | Small file (4.5 MB) and fast save/load |
| Idle working-set trim after 60 s | Near-zero RAM footprint when not in use (user's choice) |
| No parallel scan, no results during the first scan, no single-letter index | Rejected by the user: complexity and memory for rare cases |

## 7. Locked in (do not change without discussing)

- Name-only search; local only; Windows only; NTFS is the primary source.
- Rust, `windows-sys`, native Win32 UI (no Electron, WebView or .NET).
- The index data model (parallel arrays, name table with interner, sorted record map,
  flags byte with location class, `DELETED` and `SKIPPED`).
- Following the USN journal; auto-creating journals at 32 MB / 8 MB.
- Clutter skipping on by default, contents only, with the rule kinds in section 5.3.
  Rule changes must bump `SKIP_RULES_VERSION`.
- "New entries only if the parent folder is indexed."
- Compaction policy (about 2%, non-blocking).
- Save policy (hourly plus normal quit; no shutdown handler).
- Idle trim after 60 s.
- Snapshot at `%LOCALAPPDATA%\better_search\index.bin`, format v3 (bump `VERSION` when
  the format changes).
- UI requirements: tray icon, Alt+Space hotkey, right-edge hover zone with a slide-in panel.
- Decided for Phase 3 (see below): background service first; hide other users' private
  folders from each user; include removable non-NTFS drives (as the last step of
  Phase 3); exclude network drives.

---

## 8. Next phases

### Phase 3: background service (next)

**Goal:** the index lives in a Windows service that starts with Windows. The search
window (Phase 4) talks to it and needs no admin rights, so there is no UAC prompt except
once at install.

**Steps:**

1. **Engine as a library.** Move the load / scan / catch-up / watch / save / trim logic
   out of `crates/cli/src/live.rs` and `memory.rs` into a new shared crate (for example
   `crates/engine`, package `bs-engine`). Remove the `println!` calls from it; report
   progress through a log callback or a log buffer instead. The `bs` console tool keeps
   working on top of the library, for testing and benchmarks.
2. **Service binary (`bs-service.exe`, new crate `crates/service`).**
   - Uses the Service Control Manager API via `windows-sys`
     (`StartServiceCtrlDispatcherW`, `RegisterServiceCtrlHandlerExW`,
     `SetServiceStatus`). Handles stop and shutdown cleanly (stop watchers, save once on
     a normal stop).
   - Runs as `LocalSystem`, so it can read the MFT and journals.
   - Loads the snapshot, catches up, then follows the journals. Same save, compaction
     and idle-trim behaviour as today.
   - The snapshot moves to a machine-wide location such as
     `%ProgramData%\better_search\index.bin`, readable only by SYSTEM and administrators
     (it contains all users' file names).
   - Logs to a small rotating file or the Windows event log.
   - A console mode (for example `bs-service --console`) for debugging without
     installing.
3. **Local connection (named pipe).**
   - Pipe name such as `\\.\pipe\better_search`.
   - Security descriptor: local interactive users only; remote access rejected
     (`PIPE_REJECT_REMOTE_CLIENTS`).
   - A small binary protocol: request = query text + result limit (+ options such as
     "include skipped folders" later); response = total match count + the best results
     (full path, is-folder flag, score). Results are capped so replies stay small.
   - Several clients at once; one thread per connection or overlapped I/O.
   - Target: well under 1 ms added to a search.
4. **Per-user privacy.**
   - The service identifies the caller with `ImpersonateNamedPipeClient` /
     `GetNamedPipeClientProcessId` and the caller's token (user SID, profile path).
   - Each user's results leave out other users' profile folders (`<drive>\Users\<other>`).
     Administrators get the same filtering by default.
   - Possible later: also hide folders whose permissions deny that user (checking
     permissions per result with `AccessCheck`).
5. **Test client.** `bs query "text"` (or a small separate tool) sends a query through
   the pipe and prints the results, so everything can be tested without the UI.
6. **Removable non-NTFS drives (FAT, FAT32, exFAT: USB sticks, SD cards).**
   - Indexed on plug-in by walking folders (`FindFirstFileExW` with
     `FIND_FIRST_EX_LARGE_FETCH`, `FindExInfoBasic`); kept current with
     `ReadDirectoryChangesW`; dropped when the drive is removed.
   - Kept in memory only (no snapshot for them).
   - Plug-in and removal detection: `RegisterDeviceNotification` or `WM_DEVICECHANGE`
     in the service, or polling `GetLogicalDrives` every few seconds as a simpler first
     version.
   - NTFS-formatted removable drives could use the journal path, but that needs care
     (the drive can disappear at any time).
   - Network drives are excluded.
7. **Measurements** on the real drives: service memory, start time, search round trip
   through the pipe, idle CPU.

**Testing the service during development:** installing a service changes the system,
so ask the user before registering it. Use a clearly named temporary service (for
example `better_search_dev`) created with `sc.exe create` from an elevated test script,
and delete it (`sc.exe stop` + `sc.exe delete`) at the end of every test run, until
Phase 5 provides a real installer.

### Phase 4: search window

- Native Win32 window in Rust, no admin rights, talks to the service over the pipe.
- **Access:** tray icon (menu: open, settings, pause, quit); global hotkey **Alt+Space**
  (`RegisterHotKey`; note that Alt+Space normally opens a window's system menu, so the
  hotkey should be configurable); a hover zone at the right screen edge that slides the
  panel in (a thin transparent always-on-top window, with a short delay to avoid
  accidental triggers).
- **Panel:** search box plus result list (virtual list, file icons from
  `SHGetFileInfo` with caching), results update on every keystroke.
- **Keys:** Enter opens the file; Ctrl+Enter (or similar) opens the containing folder
  with the file selected; arrow keys move; Esc hides the panel. A context menu with
  "Open", "Open folder", "Copy path".
- **The safety net for clutter rules:** a line such as "12 more in skipped folders"
  that one click includes; a settings page listing the skipped folders with the most
  files, with toggles to skip or unskip each (needs a way to index one skipped folder on
  demand, and to store user overrides next to the rules version).
- Settings: hotkey, hover zone on/off and side, drives to include, start with Windows.
- DPI awareness (per-monitor v2), dark and light theme following Windows.
- Memory target for the window process: a few MB.

### Phase 5: installer

- One installer file (for example MSI via WiX, or a small custom Rust installer).
- Installs `bs-service.exe` (service, starts automatically) and the window (started at
  logon for each user), and sets the pipe and snapshot permissions.
- One UAC prompt, at install.
- Clean uninstall: stop and delete the service, remove files, the snapshot folder and
  auto-start entries. Offer to keep or delete settings.
- Code signing (so SmartScreen does not warn), and a version and update check later.

### Later ideas (not decided)

- Showing results during the very first scan.
- Filters (only folders, only a drive, by extension) and simple query syntax.
- Recently opened files ranked higher.

---

## 9. Working practices for this repository

- **Checks before every commit:**
  ```powershell
  cargo fmt --all --check
  cargo test --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo build --release
  ```
  At `913618b`: 53 workspace tests pass (index crate 30, query crate 16, 7 in the other
  crates), clippy clean.
- **Measuring on real drives:** the assistant's terminal is not elevated. Test scripts go
  in `D:\better_search\target\admin_run\` (ignored by git) and are run with
  `Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList
  '-ExecutionPolicy','Bypass','-File','<script>'`, writing results to text files that are
  read afterwards. The user approves each UAC prompt. Latest scripts: `run6.ps1` (rescan
  plus live test), `run7.ps1` (live marker test).
- **Live tests must run outside `D:\better_search\target`:** that folder sits next to
  `Cargo.toml`, so the clutter rules skip it. Use a folder such as `D:\bsprobe_live` and
  delete it afterwards.
- **Shell:** Windows PowerShell 5.1. No `&&` / `||`; use `;` and `$LASTEXITCODE`. Run
  cargo through `cmd /c "... 2>&1"` so stderr output does not produce a false error
  exit code.
- **Commits:** write the message to `.git\COMMIT_DRAFT.txt`, run
  `git -C D:\better_search commit -q -F .git\COMMIT_DRAFT.txt` in a separate step, then
  delete the draft. Messages end with
  `Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>`.
  Do not change the git identity. There is no remote; do not push.
- **Style:** match existing code; comments only where the reason is not obvious; plain,
  direct explanations for the user, including trade-offs, before big decisions.
