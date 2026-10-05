# better_search: project record

This document records what has been built, how it works, why each decision was made,
what is settled, and what comes next. It is the hand-off point for anyone (or any new
chat session) continuing the work. Keep it current when decisions change.

Last updated with release `v0.2.3`. `docs/HANDOFF.md` is a shorter, self-contained summary
of this record for starting a new chat.

Current state, in short:

- Phases 1–5 are done. These are the index, live updates, the background service with
  its pipe and privacy rules, the native search window, and the installer.
- Since the installer, the work has gone into the window. better_search now replaces
  Windows Start and Explorer search:
  - app-first ranking and open history;
  - Microsoft Store apps, real icons and thumbnails;
  - a redesigned two-line results list;
  - the `ext:` and `in:` filters;
  - Settings pages and power commands;
  - Win+S, scoped to the Explorer folder in front;
  - typing in Start;
  - turning Windows search off (section 5.11);
  - updates from GitHub releases, with one button that checks, verifies and installs
    (section 5.12).
- The pipe protocol is **version 2**. Version 2 adds the options byte and a hidden-match
  count, so system and app-folder matches are hidden by default (`78efd7a`). The skip
  rules are version 8.
- Searching a letter or `c:` now finds the drive itself: pass 2 no longer drops volume
  roots, and the window names them from the volume's label and opens them in Explorer.
- On the development machine, better_search is **not installed** (2026-10-05). The last
  install (0.2.5) was removed with its own uninstaller to test the removal end to end, and
  the machine was left with **Windows search on** (`DisableSearch` policy absent,
  `WSearch` running and rebuilding its index), which is the state an uninstall must always
  leave behind.
- Since 0.2.5, the panel shows what each side costs, measured live: the service answers a
  read-only sizes request, and Settings quotes Windows search's own memory and index size
  next to better_search's (section 5.11a). The first-run offer itself no longer quotes
  numbers: it is three plain sentences that explain what indexing is, ask the question,
  and name the cost and the undo, with "No" as the default answer.

The temporary live resource display added at `06ce1f7` was removed at the user's
request before packaging (`f8bee52`). Its isolated test measured 24.1 MiB of combined
private memory (21.8 MiB service, 2.3 MiB window) and 6.9 MiB on disk for the
snapshot, the log and the three release binaries (`target/admin_run/stats_client.txt`).

---

## 1. Goal

The fastest and leanest file and folder **name** search for Windows:

- Finds files and folders by name as you type, in a few milliseconds.
- Uses as little RAM, CPU and disk as possible. Idle CPU should be zero.
- Local only. Nothing leaves the PC.
- One-click install, and instant access from anywhere: a tray icon, the **Alt+Space**
  hotkey, and Win+S. The panel opens as a square at the right screen edge.
- Target users: ordinary people with 1–2 TB drives, not only developers.

- Since Phase 5, it also replaces Windows Start and Explorer search. It finds apps
  (Store apps too), Settings pages and power commands. It opens on Win+S, scoped to the
  Explorer folder in front, and when the user types in Start. It can turn Windows
  search off.

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
| `9c479c7` | Phase 3 steps 1-5: engine crate, `bs-service.exe` (SCM), named pipe, per-user privacy, `bs query` |
| `fb317c6` | Project docs and README for Phase 3 steps 1-5 |
| `11970b2` | Security fixes from an audit: planted data folders are moved aside, the client refuses a pipe the service did not create |
| `c5a9a6d` | Docs for those fixes, plus corrections the audit found in the Phase 3 docs |
| `8cb2f4c` | Profile folders outside `Users` are private too; users epoch fix for a renamed users folder; missing tests |
| `939d0ff` | Record the Phase 3 security audit follow-up in the project docs |
| `c626267` | Phase 4 native search window, tray, hotkey, hover, settings, and deferred backend decisions |
| `7d9de03` | Record the Phase 4 implementation commit in project history |
| `734b61b` | Memory-only removable FAT-family indexes, fixture tests, and final fixed-drive service measurements |
| `ba13883` | Record the removable support commit in project history |
| `06ce1f7` | Read-only version 2 stats request and temporary live memory/disk display in the search window |
| `b316126` | Record the live stats commit |
| `f8bee52` | Remove the temporary live stats display again |
| `d7c94e5` | Phase 5 installer (`tools/installer`) and its docs |
| `68a0707` | Record the Phase 5 install/uninstall end-to-end test |
| `05564b7` | Modern look for the window, still pure Win32 (manifest, colours, fonts) |
| `c1f59ac` | Rank app-like matches first and quiet developer clutter |
| `aceea11` | Demote program internals and boost Start Menu shortcuts |
| `58e16ad` | Open history (frecency): files the user opens rank higher |
| `7663a83` | Word-initial (acronym) matching for short searches |
| `03a2ecb` | Start Menu and installer ranking tuned on the real index |
| `78efd7a` | Pipe protocol v2: system and app-folder matches hidden by default (Ctrl+H); history seeded from Windows Recent |
| `841e4dc` | Rank by kind (apps, documents, media, folders, code); exact names win |
| `a66bb7a` | Folder-word matching, `ext:` filter, ranking fixes |
| `334c87b` | App display names, real icons and thumbnails |
| `890f744` | Microsoft Store apps through `shell:AppsFolder`; WindowsApps counted as app files |
| `3fef8b5` | Window redesign: two-line owner-drawn rows, sections, highlights, `draw.rs` |
| `0310085` | Win+S hook, `in:` folder scope from Explorer, Settings pages and power commands |
| `438c7da` | Typing crash fix, Windows search off/on with a SearchHost watcher, typing in Start |
| `ec30dde` | `docs/HANDOFF.md` |
| `7b7810a` | Bring the project record up to date with the window work since Phase 5 |
| `9a79804` | Default the window to a 9:16 right-edge panel at 70% of the screen |
| `e65a578` | Make the panel square and remove the edge-hover pop-in |
| `21b0540` | Drop the legacy hover/left keys from the settings test |
| `c79bc7a` | Settings gear, one-click Windows search switch, indexing wait, one panel per session |
| `d9b81c6` | First-run offer to turn Windows search off, with the reasoning |
| `9227386` | GitHub releases, in-place upgrades, and a Check for updates button (v0.2.0) |
| `02d0b37` | Record the release and update work in the project docs |
| `85669b7` | Run cargo and gh through `cmd.exe` in the release script |
| `ddbf755` | Clear renamed-old images after an upgrade; version 0.2.1 |
| `9c91fdf` | Leave Program Files clean on uninstall; adopt a folder of our own leftovers (v0.2.2) |
| `2c447e0` | Quit the panel on uninstall so its image is released; version 0.2.3 |
| `16640cb` | Start the panel after install, wait for elevation, report in message boxes; version 0.2.4 |
| `3021d1a` | The first-run offer quotes measured numbers, turning Windows search off frees its index, the uninstall restores search and removes per-user data; version 0.2.5 |

## 4. Current results on the development machine

Measured with an elevated run of `bs --bench` and of the service after `9c479c7`:

| Measure | Value |
|---|---|
| Files and folders scanned | 3,981,149 |
| Kept in the index (clutter skipped) | 609,686 (85% skipped) |
| Entries hidden by clutter rules | 3,371,463 (node_modules 2.1 M, System Volume Information 255 K, winsxs 206 K, ...) |
| First full scan | 53-61 s under the service (C: 20.7-25.6 s, D: 29.4 s, E: 2.2 s; the HDD is the limit) |
| Normal start (load snapshot + catch up) | 134 ms load plus 13 ms catch-up under the service; 158 ms for `bs` |
| Index memory | 18.8 MB (names 8.9, entries 5.3, change lookup 2.3, name lookup 2.2) |
| Privacy map | 0.6 MB, built in 7 ms (once per index state), extended in 0.2 ms (per change) |
| Service process memory | 21-22 MB private at ready, 25 MB working set |
| Snapshot on disk | 4.5 MB, saved in 270-530 ms |
| Search through the pipe (same query repeated) | 0.6-1.1 ms (readme, notepad, "config json"), 3.0-3.4 ms (png), 9-11 ms (single letter "e"); see the note below |
| Fresh search (`bs --bench`, median) | readme 2.2-2.5 ms, "e" 7.7-8.9 ms (the same as `913618b` side by side; see the note below) |
| Pipe overhead | 0.1-0.2 ms over a search; a connection costs 0.2 ms |
| Typing a word | first keystroke 7-11 ms, 1-2 ms from the fourth letter on (under 1 ms for long words with few matches) |
| Idle CPU | 0 while nothing changes on the drives; 359 ms per 90 s while builds and file changes were happening (journal updates, by design) |
| Service stop (save included) | 680 ms; restart to ready 319 ms |

The machine state moves these numbers by up to 25%: an earlier bench of the same index gave
"e" 7.1 ms and a 60 ms start, and the same binary gave 8.9 ms and 158 ms in a later run
while the editor and this session were also running. Compare runs within one session
rather than across sessions.

The pipe numbers come from `bs query --bench`, which repeats each query 15 times on one
connection and reports the median. From the second repeat on, the connection's session
re-checks only the names that matched before, so these are best-case numbers, not the
cost of a fresh search. A fresh search through the service has not been measured yet.
Fresh searches are **not** slower than at `913618b`, although the saved runs made it look
that way (readme 0.9-1.0 ms and "e" 6.0 ms then, 2.2-2.5 ms and 8.9 ms at `9c479c7`).
Both binaries were run alternately in one session (`run13.ps1`, `18_real_*.txt`, three
runs each on the real index): readme 2.3 ms for both, "e" 8.3-8.7 ms old and 7.7-8.5 ms
new, and the other queries equal within 0.3 ms. The same held on 3 million generated
entries without elevation (`18_synth_*.txt`, every query within about 10%), where typing
was faster in the new binary. The lower numbers of the earlier runs came from a quieter
machine, so always compare binaries side by side.

History of the main numbers, to show what each change bought:

| After commit | Index RAM | Snapshot | Start | Typical search |
|---|---|---|---|---|
| `0ba9ffc` (everything indexed) | 92 MB | – | 0.35 s | 4–6 ms |
| `4b12a23` (clutter skipped) | 35 MB | 7.1 MB | 0.15 s | 2–3 ms |
| `aca0454` (sorted lookup, name table back) | 23.5 MB | 6.4 MB | 87 ms | 1.2–1.9 ms |
| `913618b` (universal clutter rules) | 18.4 MB | 4.5 MB | 60 ms | 0.7–1.3 ms |
| `9c479c7` (service, pipe, privacy) | 18.8 MB | 4.5 MB | 134 ms (service) | 2.2–2.5 ms fresh (`bs --bench`, see above), plus 0.1–0.2 ms pipe |

**Phase 4 window smoke test, not a Phase 3 final measurement.** Saved reports
`target/admin_run/ui_smoke.txt` and `ui_client.txt` measured the release window:
2,387,968 bytes private and 16,175,104 bytes working set after showing the
missing-service state and exercising settings, Esc, hotkey and slide animation;
5,693,440 bytes private and 27,516,928 bytes working set while showing 200
virtual rows for `readme` through the real service (1,674 visible matches).
The shell image list and common controls account for much of the working set.
This is one session, not a side-by-side performance comparison. The temporary
service was deleted after the test, along with its ProgramData folder.

**Phase 3 final service measurement after the removable overlay.** The approved
temporary run `target/admin_run/phase3_final.ps1` produced ignored reports
`phase3_final.txt` and `phase3_pipe.txt` on the real C:, D:, E: drives. No FAT/exFAT
device was attached, so this measures the monitoring overhead without a removable
index, not removable-drive scan time or memory.

| Measure | Current release build | Earlier `9c479c7` reference |
|---|---|---|
| Cold full scan to ready | 55.32 s for 609,088 entries | 53–61 s |
| Warm start after clean stop | 137.2 ms inside service, 304 ms wall | 134 ms inside service |
| Index memory at ready | 18.6 MB after warm start | 18.8 MB |
| Service process at warm ready | 21.5 MiB private, 25.7 MiB working set | 21–22 MB private, 25 MB working set |
| Repeated pipe query `readme` (15-run median) | 1.2 ms round trip, 1.1 ms service | 0.6–1.1 ms service |
| Repeated pipe query `e` (15-run median) | 9.3 ms round trip, 9.2 ms service | 9–11 ms service |
| Pipe overhead | 0.1–0.2 ms for 20 hits; 0.8 ms for 1,000 hits | 0.1–0.2 ms for 20 hits |
| Idle CPU | 16 ms of CPU time over 60 s without queries | 359 ms over 90 s *with file changes* |

The old and new release builds were alternated in one session for fresh-search
comparisons in `18_real_old_*.txt` / `18_real_new_*.txt` (section 4 above).
The service reference row is from a different session and is **not** a controlled
side-by-side A/B comparison; the drive contents and machine load changed. The idle
CPU samples have different activity and cannot support a direct speedup claim. The
current run created and removed only `better_search_dev` and its own ProgramData
folder; both were confirmed absent afterward.

---

## 5. Architecture

A Cargo workspace with eight crates:

| Crate | Package | Purpose |
|---|---|---|
| `crates/ntfs` | `bs-ntfs` | Reads NTFS volumes: full file list and the change journal |
| `crates/index` | `bs-index` | The compact in-memory index, clutter rules, snapshot format |
| `crates/query` | `bs-query` | Ranked, parallel, case-insensitive name search |
| `crates/engine` | `bs-engine` | Keeps the index loaded, current and saved; per-user profile map |
| `crates/pipe` | `bs-pipe` | Message format of the service's named pipe, and a client for it |
| `crates/service` | `bs-service` (binary `bs-service.exe`) | Background service: owns the index, answers searches |
| `crates/cli` | `bs-cli` (binary `bs`) | Console tools: index and search locally, `bs query` through the service |
| `crates/ui` | `bs-window` (binary `bs-window.exe`) | Unelevated native Win32 panel, tray, hotkey, per-user settings |

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
  `Recovery`, `node_modules`, `.git` and similar. Ranked lower. Also folders named
  `bun`, `.bun`, `vcpkg` or `certs`, and developer toolchains installed at a drive root
  (`msys64`, `msys32`, `msys`, `cygwin`, `cygwin64`, `mingw64`, `mingw32`, `w64devkit`,
  `strawberry`, `devkitpro`), so their share/zoneinfo and package data stop crowding
  ordinary name searches.
- `AppFiles`: a program's own files, recognised by shape alone with no app names:
  two or more folders below `Program Files\<app>` (or `Program Files (x86)`), Squirrel
  `app-<digit>…` version folders, and dot-folders directly in a profile (`.vscode`,
  `.docker`). The app's own folder and the files directly inside it stay `Normal`.
  Ranked lower (−30) but still found. Programs installed elsewhere are not guessed at.
- `StartMenu`: a `Start Menu\Programs` folder and everything below it, even though
  it sits inside `ProgramData` or `AppData`, except under `Windows\ServiceProfiles`
  (service accounts). Shortcuts in it rank higher (+50), because installed programs
  register them there; its folders are neutral.
- `Normal`: everything else.

The class is stored in three flag bits (two at bits 2–3, one at bit 6) so older saved
indexes read the same. `SKIP_RULES_VERSION` is 8; each change to it triggers one rescan.

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

**Users epoch.** `Index::users_epoch()` counts changes that can move a result between
users' profile folders, plus compactions (which renumber entries). The service's
privacy map is rebuilt when it changes and extended otherwise, so the map costs
something only when ownership really changed. Inside `apply`, that means: a users
folder or a profile folder (`<drive>\Users\<name>`, or one of the folders given to
`set_profile_folders`) was created, renamed, moved or deleted (checked before and after
the change, so a users folder renamed away counts too), or an entry entered, left or
moved between profile folders. Everything else (a new file inside a profile folder, for
example) inherits its owner and costs nothing.

**Profile folders elsewhere.** Windows can keep a profile anywhere (`ProfileImagePath`
in the registry). `Index::set_profile_folders` takes those paths; the ones not directly
in a users folder are looked up one level at a time (a scan per level, a few ms, only
when the list changes) and then count as profile folders exactly like the ones in
`Users`. Their entries are carried through compaction. A listed folder that is not in
the index yet is looked up again at the end of a batch in which a folder with its name
was created or renamed. The list is not saved in the snapshot; the service sets it.

### 5.3 Clutter skipping

About 85% of all files on a typical PC are ones nobody searches by name: package
folders, build output, caches, Windows component stores. Leaving them out cut RAM,
snapshot size, start time and search time several times over.

**Principle:** only the **contents** of a clutter folder are left out. The folder itself
stays searchable (you can still find `node_modules` or `WinSxS`). A skipped folder has
the `SKIPPED` flag; nothing below it is in the index.

**Rules** (all in `crates/index/src/lib.rs`), current version `SKIP_RULES_VERSION = 8` (versions 4–8 added the location and app-file rules described in 5.2 and 5.11):

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
  Extras: a name that is exactly the query plus one extension is an exact stem match;
  when that extension is **launchable** (`.exe`, `.com`, `.bat`, `.cmd`, `.msi`, `.lnk`,
  `.url`, `.appref-ms`) it becomes the highest tier, so the app a user means
  (`factory.exe`) outranks folders and installers that only share the prefix.
  Launchable extensions elsewhere get +10, except installers (`.msi`, or a name containing `setup`, `install` or `unins`), which get −20; `.dll`, `.mui`, `.tmp`, `.log`, `.etl`,
  `.cat`, `.manifest`, `.pf`, `.pyc` and the certificate/runtime types `.pem`, `.pid`,
  `.crt`, `.key`, `.pdb`, `.lib`, `.obj`, `.ilk` get −10; longer names lose a little.
- **Pass 2, per entry:** name score plus location (`UserContent` +25, `StartMenu` +50 for files and 0 for its folders, `AppFiles` −30, `Noisy` −45),
  folders +3, hidden −20. Each thread keeps only the best `limit` hits in a small heap,
  so the full match list is never built, even for millions of matches. Ties go to the
  lower entry number, so results are deterministic.
- **Drive roots are results.** A volume root (`C:`) has no parent entry, and pass 2 used
  to drop every parentless entry ("not useful results"), so no drive could ever be found.
  A root now passes like any folder when its own name carries every term (`c`, `c:`); it
  never borrows folder words, because it has no folders above it, and it is not below its
  own `in:` scope (`in:"D:\"` keeps entries below D:, not D: itself). The window shows the
  volume's label ("Data (D:)", or "Local disk (C:)" when it has none), the shell's drive
  icon, and a second line saying the drive kind; Enter opens the drive in Explorer.
- **Acronym fallback:** a query of one ASCII word, 2–6 letters, with fewer than 5,000
  ordinary matches also finds names whose word initials spell it exactly (extension
  ignored; words split at non-alphanumeric characters and camelCase). `vsc` finds
  `Visual Studio Code.lnk`, `mrv` finds `MyReportViewer.cs`. An acronym match scores 60
  (+10 launchable, −10 demoted type), then location applies, so a Start Menu shortcut
  reaches 120. Results are merged with the ordinary ones. The scan reads every unique
  name but the first-byte test skips nearly all of them; above the 5,000-match limit it is
  skipped, so busy queries cost nothing extra (synthetic 2M entries: png, exe and readme
  unchanged at 5–6 ms). Narrowing does not apply to it.
- **Type-ahead narrowing (`Session`):** when the new query only adds characters to the
  previous one, only the names that matched before are re-checked. The cache resets when
  the index `generation` changes. Narrowing keeps a match list only up to 8192 names:
  re-checking remembered names costs about 80 ns each (a per-name call), while the SIMD
  scan over the whole name buffer costs about 5 ns per name, so long lists are cheaper
  to search from scratch. Match lists are also only built when they are going to be
  kept (`collect_matches`), so a broad query in a session runs at the same speed as a
  fresh one, and the service does not allocate megabytes per keystroke per client.
- **Zero-match early exit:** if no name matches, pass 2 is skipped.
- Parallelism: `rayon`.

### 5.5 Snapshot (`crates/index/src/snapshot.rs`)

- Path: `%LOCALAPPDATA%\better_search\index.bin` for the console tool;
  `%ProgramData%\better_search\index.bin` for the service (see 5.7).
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

### 5.6 The engine (`crates/engine`)

Both the console tool and the service are thin shells around `Engine`. It owns the
index, keeps it current, saves it and serves searches:

- `Config` chooses the NTFS drives (empty means all fixed NTFS drives), the snapshot
  path, whether to ignore the snapshot (`rescan`) and whether to skip clutter. The save
  interval and trim delay are constants. A `Log` callback (an `Arc<dyn Fn(&str)>`) replaces printing, so the
  console tool prints and the service writes to its log file; the crates never print.
- `Engine::open` loads the snapshot and catches up from the journals, or scans the
  drives when there is no usable snapshot; `Engine::from_index` wraps an index that has
  no drives behind it (a walked folder, test data), so it gets no live updates and no
  snapshot; `start` spawns the watcher and saver threads; `read` gives a read lock for
  searching (the caller runs `Session::search` or, in the service,
  `Session::search_filtered` with the privacy filter); `touch` records that a search
  happened, which postpones the idle trim; `save` saves now; `shutdown` stops the
  threads and optionally saves.
- `shutdown` must be able to interrupt a journal read that is blocked in the kernel, and
  a blocked `ReadFile` never checks a stop flag. `shutdown` therefore calls
  `CancelSynchronousIo` on each watcher's thread handle (taken from its `JoinHandle`),
  repeating it every 5 ms because a cancel can land just before the thread enters the
  wait. That is why a service stop is fast (a few hundred ms) instead of waiting for the
  read timeout.
- **Shared state:** `Shared` holds the index in an `RwLock`. Searches take a read lock.
- One **watcher thread per drive** follows its journal and applies changes in batches
  under the write lock.
- A **maintenance mutex** is taken by writers and by the saver before the write lock.
  So while a long read-locked job runs (compaction or save), no writer is queued on the
  `RwLock`. A queued writer would make new searches wait.
- **Saver thread:** wakes every 15 s. Saves once an hour if anything changed. When
  saving, it first compacts if `needs_compaction()`: builds the clean copy under a read
  lock, swaps it in under a brief write lock, and frees the old copy outside the lock.
- **Idle trim:** after 60 s without a search, `EmptyWorkingSet` lets Windows page the
  index out of RAM; the first search after that reads it back.
- No save at process exit in the console tool, but `:q` calls `shutdown(true)`, which
  saves first. The service saves on stop and skips the save on a machine shutdown
  (`shutdown(false)`), see 5.7. Anything missed is replayed from the journal at the next
  start.

### 5.7 The service (`bs-service.exe`, `crates/service`)

```
bs-service.exe            Run as a service (started by the SCM)
bs-service.exe --console  Same thing in a terminal, for testing
```

- Registered as a service with the SCM (Windows service control manager), so it starts
  at boot as **LocalSystem** and needs no user session. `StartServiceCtrlDispatcherW` is
  called from `main`; the SCM handler accepts `STOP` (save, then exit) and
  `SHUTDOWN` (exit without a save, because the machine is going down anyway) and reports
  `SERVICE_STOP_PENDING` with a wait hint while it saves, so an installer or the user
  never sees "did not respond".
- The data folder is `%ProgramData%\better_search`. It is created (or re-secured) on
  every start with a **protected** DACL (no inherited entries) that gives only SYSTEM
  and administrators full control, and owner Administrators
  (`O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)`). Other users cannot list the folder, read
  `index.bin` or `service.log`, create files in it or rename it (all four verified from
  a non-elevated shell against the running service, `17_user_probes.txt`).
- **A planted folder is never used** (`security::secure_dir`). Every user may create
  folders in `%ProgramData%`, so before the service first runs, a user could put a
  junction there (the service would then set its DACL on, and write the index into,
  whatever the junction points at) or a folder they own (an owner can always rewrite
  the DACL and read the index later). On every start the service opens the folder
  without following links and checks it: a junction or other reparse point, a file, or
  an owner other than SYSTEM or Administrators makes it untrusted. Otherwise the owner
  and DACL are set **through that handle**, so a folder swapped in between the check
  and the change cannot receive them. Then the entries inside are checked (no links,
  no hard-linked files, trusted owners only). An untrusted folder is renamed to
  `better_search.untrusted-<ms>-<pid>` (the link itself for a junction, so nothing
  behind it is touched), one log line says why, and a fresh folder is created; this is
  retried up to four times. The moved-aside folder stays for an administrator (or the
  uninstaller) to delete, because its contents are not the service's to judge.
  Verified in `run12.ps1` (`17_service.txt`): a junction to `D:\bsprobe_decoy` planted
  by a normal user was moved aside and the decoy kept its one file and identical
  permissions; a trusted folder was reused on restart (ready in 308 ms); a folder
  handed to the user (owner plus full control, a planted file) was moved aside and
  replaced.
- Log: `%ProgramData%\better_search\service.log`, one line per event with a timestamp,
  rotated at about 1 MB keeping one old copy. It records single lines only: start,
  per-drive scan results, skipped-entry summary, save, ready, one line per client
  (user SID and profile path), stop, errors. Queries are not logged.
- While the index loads or scans, searches answer `Status::Loading` and the client says
  "the index is still loading", instead of failing. Installation state (first full scan)
  is the only time this happens for more than a moment.
- The console mode exists so the service can be tested end to end without registering
  it. Registering a service is a machine-wide change, so it is always done with the
  user's approval; during development the scripts in `target/admin_run` used a
  temporary `better_search_dev` service and deleted it afterwards.

### 5.8 The named pipe (`crates/pipe`)

- Name: `\\.\pipe\better_search`. One message per request and reply, message mode, so a
  client cannot read a partial reply; remote clients are rejected
  (`PIPE_REJECT_REMOTE_CLIENTS`).
- **Who may connect:** the pipe's security descriptor
  (`O:BAD:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12018b;;;IU)`) denies a network
  logon outright and allows `SYSTEM`, `BUILTIN\Administrators` and `INTERACTIVE` (each
  logged-on interactive user). Users get read and write (query text in, reply out) but
  **not** `FILE_CREATE_PIPE_INSTANCE`, so while the service runs, a program started by
  a user cannot add its own instance of `\\.\pipe\better_search` and receive other
  users' queries. The service also creates its first instance with
  `FILE_FLAG_FIRST_PIPE_INSTANCE`, which fails if anyone got there first. (The denied
  create was not tested separately; it follows from the descriptor.)
- **The client checks the server.** While the service is stopped, any program may
  create a pipe with that name. The pipe is therefore owned by Administrators, which an
  ordinary user cannot make the owner of anything, and `Client::connect` reads the
  owner right after opening the pipe and refuses anything other than SYSTEM or
  Administrators, before a query is sent. The fake server learns only who connected
  (identification level, see 5.9). Verified: a pipe named `better_search` created by
  the normal user made `bs query` fail with "\\.\pipe\better_search is served by a
  program that is not the better_search service (the pipe is owned by S-1-5-21-...)";
  a unit test covers the same. An elevated program can still pass the check, which is
  fine: an administrator can read the index anyway.
- **Request (version 2):** `version u8 · kind u8 (1 = search) · limit u16 · options u8
  (bit 0 = include system and app folders) · query UTF-8` (the query is the rest of the
  message, at most 4096 bytes in total; the limit is capped at 1000). By default the
  query crate counts matches in `Noisy` and `AppFiles` locations as *hidden* instead of
  ranking them. **Reply:** `version u8 · status u8 · total matches u32 · hidden matches
  u32 · search time µs u32 · hit count u16`, then per hit `score i32 · flags u8 (1 = folder) · path length u16 · path
  UTF-8`. Status is `Ok`, `Loading`, `BadRequest` or `Denied` (the caller could not be
  identified). Little endian, size-checked on both sides, with round-trip and
  truncation tests. Every message carries the version and a mismatch is rejected.
- **Client** (`bs-pipe`): connects, then `TransactNamedPipe` per query, reading the rest
  of a reply if it did not fit in one buffer (`ERROR_MORE_DATA` handshake), retrying on
  `ERROR_PIPE_BUSY` for a few seconds. Overhead measured from the console tool:
  0.1-0.2 ms per query (connection 0.2 ms), against a service-side search of 0.6-11 ms
  (repeated queries, see section 4).
- **Sizes request (since 0.2.5, same version):** `version u8 · kind u8 (2)` in, and
  `version u8 · status u8` plus nine `u64` sizes out in the order of `StatsReply`:
  service private and working set, index heap, index and log on disk, the service binary,
  entry count, and Windows search's memory and disk. A size that is not known yet is
  `u64::MAX`, which decodes to `None`: unknown is not the same as zero, and a test pins
  that a real zero reads back as a measurement. It needs no viewer lookup, because it says
  nothing about any file, so a window that may not search may still ask what things cost.
  The service is the only part of better_search with the rights to read
  `C:\ProgramData\Microsoft\Search`, so the Windows search figures come from it.
- **Connections:** one blocking thread per connection, at most 64 at a time for all
  users together.

### 5.9 Per-user privacy (`crates/engine/src/profiles.rs`)

The point: a normal user must not see file names in other users' profile folders.
Administrators are filtered the same way, as the plan decided, so an administrator's
everyday search does not show other people's files. For administrators this is a
courtesy, not a security boundary: they can read the index file or the profile folders
directly.

- **Who is asking:** on each connection the service calls `ImpersonateNamedPipeClient`
  at **identification** level (enough to read the token, not enough to act as the user),
  opens the thread token, reads the SID, then looks up
  `ProfileList\<SID>\ProfileImagePath` in the registry to get the profile folder. The
  token is read once per connection and the connection then serves many queries, as the
  search window will. Any failure to read the token fails the connection (fail closed),
  and only the user's SID and profile path are kept in memory.
- **Every profile on the machine:** before a search, at most every 10 s, the service
  lists all `ProfileList` entries (without LocalSystem, LocalService and NetworkService)
  and passes their paths to the index when the list changed (see "Profile folders
  elsewhere" in 5.2). So a profile outside `<drive>\Users` is private too, and one
  created while a search window stays connected becomes private within 10 s. The log
  gets one line with the number of paths when the list changes.
- **The map** (`Profiles`): one byte per index entry. The byte is a slot number: 0 means
  "no profile owns this", 1-253 are profiles (a folder name for every folder directly
  inside a `Users` folder at a drive root, and the whole lowercased path for each
  profile folder elsewhere, which cannot collide with a name because it contains a
  backslash), 254 means the machine has more profiles than slots (those folders stay
  hidden) and 255 is "not classified yet", which only exists while the map is being
  built. A user is allowed the slot named `public`, the slot named after their own
  profile folder, and the slot whose path is their own profile path.
- **Judging an entry:** by its **parent's** slot, not its own. So `C:\Users` is visible,
  the profile folder itself is visible (its parent is `C:\Users`), and everything below
  it is hidden. A folder that happens to have the caller's profile name on another drive
  is visible too, which is what a user expects when they search for a folder name and
  matches how Windows treats user folders that were once that user's profile. A profile
  folder renamed to something else stops being treated as the owner's, and hidden entries
  cannot leak through a parent in another profile's tree.
- **Where it applies:** the filter runs inside pass 2 of the search
  (`search_filtered`), not on the finished hits. Hidden entries are therefore missing
  from both the hits and the match count, so "1,673 matches" means 1,673 matches the
  caller is allowed to see, not a first page with a padded number behind it.
- **Cost:** the check is one byte read per entry considered, about 0.2-0.6 ms on the
  real index; the map builds in 7 ms (0.6 MB). Builds happen only when
  `users_epoch()` changes (see 5.2) and are extensions otherwise (0.2 ms), so the map is
  built about once per service run. On the real index, the console tool (map for
  `C:\Users\hp`) hid 77 of the 351,791 matches for `e` (the machine's `Default` profile)
  at no measurable cost, and through the service a `ntuser` search returned no hits from
  `C:\Users\Default` while the caller's own files stayed visible.
- **Tested outside `Users`** (`run14.ps1`, `19_*.txt`): with a temporary `ProfileList`
  entry for a made-up SID pointing at `D:\bsprobe_profile\dora` (deleted afterwards), a
  normal user's search stopped showing the file in that folder within the 10 s recheck,
  and also did not show a file created inside it later or a folder moved into it, while
  a file next to the profile folder stayed visible.

### 5.10 The console tool (`bs`, `crates/cli`)

```
bs                     Index all fixed NTFS drives (needs "Run as administrator")
bs C D                 Only these drives
bs --walk <FOLDER>     Walk a folder instead (no admin needed; no live updates)
bs --synthetic <N>     N generated fake entries, for benchmarking
--bench                Run a fixed set of timed queries and exit
--rescan               Ignore the snapshot and read the drives again
--all                  Do not skip clutter
-n <COUNT>             Number of results to show (default 20)
bs query <TEXT>        Search through the running service (see 5.8)
bs query --bench       Measure round trip, service time and overhead through the pipe
```

`bs` uses the same engine as the service, with the snapshot in `%LOCALAPPDATA%` and no
privacy filter (the user started it, so it is their own data; the service exists so a
search does not need admin rights at all). Interactive prompt: type to search;
`:changes` shows recent journal changes, `:stats` shows memory use, `:save` writes the
snapshot, an empty line or `:q` quits (and saves). Benchmarks and progress lines are
silent under `--bench` so the numbers are not disturbed. `bs query` needs no admin
rights: it prints the same hits the service would return, with `-n` to change the limit,
and reports a clear message when the service is not running.

### 5.11 The search window (`bs-window.exe`, `crates/ui`)

- A native Win32 process with per-monitor v2 DPI awareness and no elevation. Its one
  worker thread owns `bs_pipe::Client`, keeps a connection between keystrokes for type-ahead
  narrowing, coalesces pending edits and posts results to the window thread. A serial
  number drops stale replies. Failed connections retry on later searches; loading and
  missing-service states retry while visible. Denied and bad-request states are distinct.
  It does not read the service's protected data folder.
- **Look:** a small app manifest (`crates/ui/bs-window.manifest`, embedded by
  `crates/ui/build.rs` with the MSVC linker's `/MANIFESTINPUT`, no new dependency or
  runtime) activates common controls v6 and declares per-monitor DPI awareness.
  `draw.rs` holds the visual rules: a spacing scale (4/8/12/16 px), a type scale (search
  12 pt, names 10.5 pt with matched letters semibold, second lines and footer 9 pt,
  headings 9 pt semibold, Segoe UI Variable with a Segoe UI fallback, Segoe Fluent
  Icons or MDL2 glyphs), colour roles (panel, surface, hover, selected, text,
  secondary, edge, accent) on Windows 11's base colours, and the user's accent colour
  (lightened on dark). Rounded Win11 corners and a caption matching the panel. Default
  geometry (`panel_rect`): a square whose side is 70% of the work area height, centred
  vertically against the right edge, used at startup.
- The panel has three bands: a rounded search field (search glyph, placeholder via
  `EM_SETCUEBANNER`, accent underline while focused), the results, and a footer with
  the result count and a `Ctrl+Enter` hint that gives way when the count needs the
  room. Results are an owner-data, owner-drawn list view without a header (up to 200
  hits). Each row is drawn off screen and copied in one step: icon, name with
  matched letters in semibold (`rows::highlight`, including word initials), and a
  second line saying what an app is or the folder a file is in, shortened in the
  middle (`DT_PATH_ELLIPSIS`). The hovered or selected row gets a rounded fill, the
  selected one an accent pill, and both show "Show in folder" and "Copy path"
  buttons. When results have more than one kind they are grouped under headings
  (Apps, Folders, Documents, Photos videos and music, Other files), sections in the
  order of their best hit (`rows::group`); headings cannot be selected. Rows use a
  private 32 px image list (at most 600 images, reset when full): a type icon from
  `SHGetFileInfoW` with synthetic attributes (cached per extension/folder) appears
  at once, and a background STA thread (`thumbs.rs`, `IShellItemImageFactory`) then
  replaces it with the real app icon for `.exe`/`.lnk`/`.url` and a thumbnail for
  images, videos and documents. A generation counter drops requests for old searches.
  Shortcuts are shown without `.lnk`/`.url`/`.appref-ms`, and the second line
  describes apps in words (`App`, `App shortcut on the Desktop`, `Windows tool`,
  `Web link`, `App in <folder>`).
- Microsoft Store and other packaged (MSIX) apps have no shortcut file, so the index
  cannot see them. `apps.rs` reads the shell's Applications folder (`shell:AppsFolder`)
  on a background thread at start and again when a search runs more than two minutes
  after the last read, keeping entries whose app ID contains `!`. The window matches
  their names itself (exact 180, prefix 144, word start 127, substring 100, close to a
  Start Menu shortcut's scores), merges up to 20 into the service's hits, and opens
  them through `shell:AppsFolder\<app ID>`. An app that also has a Start Menu shortcut
  is shown once. `Program Files\WindowsApps` (package files) is now app files
  (`SKIP_RULES_VERSION` 8).
  Enter opens, Ctrl+Enter selects the item in Explorer, arrows navigate, Esc hides,
  and a result menu offers Open, Show in folder and Copy path.
- A tray icon provides Open, Settings, Pause and Quit. Pause suspends **window queries
  only**, not the service's journaling and saving. The global hotkey defaults to Alt+Space;
  if Windows or another app owns it, the tray still works and Settings can change it.
  A gear button at the right end of the search field opens the same Settings page, and only
  one panel runs per session (a second launch posts `WM_SHOW_PANEL` to the running window
  and exits). The screen-edge hover pop-in and its slide animation were removed by user
  decision (2026-10-05); Win+S, Alt+Space and the tray open the panel. Window positioning
  and child layout scale with the monitor DPI.
- **Open history (frecency).** The window records the path of each file it opens (not
  "open folder") in `%LOCALAPPDATA%\better_search\history.tsv` as `weight`, `last-open
  seconds`, lowercased path. A weight is the number of opens, halving every 14 days.
  After a reply arrives, the window adds `min(30, 8·ln(1 + weight))` to each hit's score
  and re-sorts the (up to 200) hits with a stable sort. Nothing changes in the pipe
  protocol, the index or the service, and history only reorders hits the service already
  returned. The file keeps at most 2,000 paths (the weakest 500 go when it overflows),
  forgets weights under 0.05, and is written by temp file and rename. Settings has a
  checkbox (`history=` in `window.cfg`, default on) and a "Clear open history" button.
- **Kind ranking.** Name-score nudges by extension: launchable +15 (installers -20), documents +14, media +9, source/config -8, generated (`.class`, `.o`, `.map`, `.lock`...) -10; folders +6; Start Menu shortcuts +60; a name equal to the query (stem or whole) gets +40 (files only, not launchable stems that already score 120). Exact stems skip the length penalty. Frecency now adds `min(60, 16 ln(1+w))`. Start Menu shortcuts with the same name in equally named parent folders collapse to the best one. `SKIP_RULES_VERSION` 6 adds `.idea`, `.eggs`, `.sass-cache`; the service rescans when the stored version differs.
- **Folder words, `ext:` and ranking fixes.** With 2 to 8 terms, pass 1 scans for each term and keeps a per-name term mask; a term missing from the name may be found in a parent folder name (volume root excluded) and counts 20 instead of its match score. Pass 2 resolves folder masks and depths through a small per-thread folder cache. Narrowing for such queries requires each new term to contain an old one. `ext:a,b` filters by extension (alone it lists that type). Exact stems with a demoted extension (`notes.log`) score 60 without the exact bonus; `.lnk`/`.url` vendor extras (`Readme`, `Help`, `<App> Website`) are capped at 20 before the Start Menu boost; word starts score 56; documents and media pay half the length penalty; ties go to fewer folders. `key`, `pages`, `numbers` left the document list; `xlsm`, `docm`, `msg`, `eml`, `one`, `vsdx`, RAW photo and more media types joined. Locations (`SKIP_RULES_VERSION` 7): `OneDrive - <org>`, Dropbox, `my drive`, iCloud, `source`, `repos`, `projects` count as user content; `certs`, `bun`, `vcpkg` are noisy only outside user content; listed `Windows` and `System32` tools rank as Start Menu entries; non-launchable files directly in `Program Files\<app>` are app files. The window imports Windows Recent opens newer than its history file on every start and records Ctrl+Enter. Measured on a 1.27M-entry profile walk: single-term searches unchanged (`readme` 2.0 ms), two-term searches 1.1 → 2.3 ms; the synthetic benchmark, whose folders are random, is about twice as slow.
- **`in:` folder scope, Win+S, Settings pages.** `in:"<path>"` (quotes optional without spaces, `/` accepted, last one wins) limits matches to entries below that folder: pass 2 finds the folder's entries once per query (name table scan plus a full-path check) and walks each candidate's parents; an unindexed folder gives no results. About 11 ms through the service on the development machine. The window installs a `WH_KEYBOARD_LL` hook on its own thread (Windows drops slow low-level hooks), swallows Win+S (not with Shift, Ctrl or Alt, so Win+Shift+S still screenshots), taps the unassigned key 0xE8 so releasing Win does not open Start, and posts `WM_APP+5`. The handler reads the folder of the foreground Explorer window's active tab through `IShellWindows` → `IServiceProvider` → `IShellBrowser` → `IFolderView` → `IPersistFolder2` (hand-written vtables; virtual folders give no scope), borrows foreground rights with `AttachThreadInput`, and shows the folder as a chip; requests are then sent as `in:"<folder>" <text>` and Store apps and Settings are left out. Backspace at the start of the field drops the chip; Alt+Space always opens unscoped. `win_s=` in `window.cfg`, default on. `commands.rs` lists about 50 `ms-settings:` pages and `windowsdefender:` with search words, plus `command:` power entries (shut down, restart, sign out ask first; sleep via `SetSuspendState`, lock via `LockWorkStation`); names score like Store apps minus 12, a keyword prefix (3+ letters) scores 120 minus 12. They form a Settings section with the Settings app's icon. Store apps and Settings rows are no longer cut by the result limit.
- **Typing crash, Windows search off, typing in Start.** The window crashed in `USER32!DrawTextExWorker` when typing fast: a bold name run cut to nothing passed `DrawTextW` a count of 0 with a dangling empty-`Vec` pointer. `draw::text` now returns early on empty text and passes a NUL-terminated buffer; `text_width("")` is 0 (test `drawing_empty_text_is_safe`). `winsearch.rs` turns Windows search off or on: the window re-runs itself elevated (`runas`) with `--windows-search off|on`, which sets or deletes the HKLM policy `SOFTWARE\Policies\Microsoft\Windows\Windows Search\DisableSearch`, runs `sc config`/`sc stop WSearch` (disabled, or delayed-auto plus start) and ends `SearchHost.exe` (it restarts within seconds when only killed). The first time the panel opens while Windows
search is still on, a one-time Yes/No dialog explains what the change saves (a background
indexer, its database on disk, machine-wide effects on other accounts and apps) and
offers it; the answer is stored as `windows_search_asked` in `window.cfg`. A one-click
Settings button drives it later: the
label says whether the press turns Windows search off or back on, and the press runs the
elevated path at once instead of waiting for Save. An `EVENT_SYSTEM_FOREGROUND` WinEvent hook ends `SearchHost.exe` if it still comes to the front while search is off and opens the window instead. The keyboard hook also swallows letters and digits typed while `StartMenuExperienceHost.exe` is in front (checked at the key press, cached per foreground window) and posts them as `WM_APP+6`; Start refuses to give up the foreground, so the window closes it with an injected Escape (only after confirming Start is in front, then waiting until it has left), takes the foreground like Win+S and appends letters that arrive within 1.5 s to the edit's current text.
- **Hidden system matches.** A measurement of the real index (`bs report <index.bin>`)
  found 94% of entries in system, app-data or program folders and 3.8% in ordinary
  locations. The window therefore asks the service to hide those by default and shows
  "N more in system and app folders (Ctrl+H)". Ctrl+H toggles showing them for the
  session (not persisted). `bs query --system` does the same in the CLI.
- **History seeding.** With no `history.tsv` yet, the window reads the Windows Recent
  shortcuts (`%APPDATA%\Microsoft\Windows\Recent`, newest 300), resolves each `.lnk`
  target from the LinkInfo block without COM, and records one open dated by the
  shortcut.
- Per-user settings live in `%LOCALAPPDATA%\better_search\window.cfg`. The theme follows
  Windows' app light/dark preference on settings changes (title bar, list and controls).
  Start with Windows is off until explicitly enabled in Settings; it only changes the
  current user's `HKCU\...\Run` value, starts the panel hidden in the tray, and requires
  no admin rights. The current service
  drives are listed as fixed NTFS candidates, not presented as an editable filter.
- The clutter safety net is **deferred by user decision**. The index has no entries
  inside skipped folders, so neither a per-query skipped-result count nor a per-folder
  unskip toggle can honestly work without more backend state. The settings page explains
  this instead of displaying invented counts. The window features above did not change
  the index arrays, flags or snapshot format. The pipe moved to version 2 only for the
  hidden-match option (`78efd7a`).

### 5.11a Saying what each one costs, and freeing the disk space (0.2.5)

The first-run offer used to explain Windows search's *architecture*: a background
indexer, its own database, machine-wide reach. That was accurate and hard to act on. So
the offer next quoted what the change is worth **on this PC**, in measured numbers. Both
editions were still too long, and the numbers made the dialog about us rather than about
the question. The offer is now three plain sentences (see 5.6's "First-run offer" in the
hand-off): what indexing is, the question, the cost and the undo. The measured numbers
live on in **Settings**, where the user can check them against Task Manager.

- **Measured, not claimed.** The service answers a read-only sizes request (section 5.8)
  and is the only part of better_search with the rights to read Windows' own search
  folders. It sums the **working set** of every `SearchIndexer.exe` (the number Task
  Manager's Memory column shows, so a user can check it) and walks
  `C:\ProgramData\Microsoft\Search` for its index size. A figure it cannot get is reported
  as unknown, never as zero, and the panel's text leaves a sentence out rather than
  filling it with a guess.
- **Settings waits for its numbers; the offer does not.** `stats::start` asks every 3 s
  (60 tries at most) and fills the Settings line once per start and again after a Windows
  search switch. The offer shows when the panel first opens visible and idle; it no
  longer waits for a reading, because its text has no numbers in it.
- **Measured on this PC** (2026-10-05, 574 K entries, Ryzen 5 3500U): the service uses
  20-21 MB private, 25-29 MB working set just after a scan, and Windows trims it to 0.8 MB
  when idle; its index is 4.2 MB on disk and 17.6 MB in memory. `SearchIndexer` uses
  8.6 MB private and 19.3 MB working set while idle on a fresh index, 29 MB while
  rebuilding, and its index folder held 34.3 MB for a full index (10-11.5 MB after a
  rebuild). **better_search does not win on memory**: the honest pitch is speed, no
  background indexing, and the disk space. The text therefore states both sets of numbers
  without a comparison it cannot support.
- **The disk comparison is index against index**, because that is what grows with the
  number of files. The Settings line quotes "34 MB of index files" for Windows search and
  "a 4 MB index" for better_search, and never claims the programs are free.
- **Turning Windows search off now frees the space.** The policy and the stopped service
  freed no disk: the index files stayed. `clear_index()` removes the contents of
  `C:\ProgramData\Microsoft\Search\Data` and keeps the folders, so Windows' own
  permissions stay; the indexer is stopped and waited for first (up to 10 s), because its
  files stay locked while the process runs. Windows rebuilds the index by itself, so
  nothing is lost. Measured: 10.9 MB and 17 files before, 0 MB and 0 files after, and
  8.3 MB rebuilt within 30 s of turning search back on.
- **Turning it back on does not wait for the indexer.** Windows brings it up in its own
  time; the panel is blocked while the elevated copy runs, and a wait long enough to see it
  would freeze the window. What is reported is the setting that makes search work again
  (`ChangeServiceConfigW` to auto start, delayed, as Windows ships it, plus a start
  request).
- **The elevated copy reports through its exit code**, the only channel it has: `0` off
  and index removed, `1` off but the index stayed, `2` on and settings restored, `3` on but
  Windows would not take its indexer back, `4` the policy could not be written. A test pins
  every mapping, so a code can never be read as a success it does not mean. The panel says
  which of those happened instead of a single "done".
- **An uninstall gives Windows search back.** Removing better_search while it had turned
  search off used to leave the machine with search disabled and nothing left to undo it.
  The uninstaller now clears the policy and restores the indexer's start type and start
  before it finishes. It also removes this account's settings and history
  (`%LOCALAPPDATA%\better_search`), which the old uninstaller left behind, and which would
  otherwise keep the "already asked" answer so a fresh install would never offer again.
- **A dismissed UAC prompt is reported.** It used to exit silently, which looked exactly
  like a program that had stopped for no reason. It now says the prompt was dismissed and
  nothing was changed.

### 5.12 Updates (`crates/ui/src/update.rs`)

Releases are published in the GitHub repository `devashish-guliya/better_search`. Each
release carries `better-search-setup.exe` and a three-line `latest.txt` (`version`,
`url`, `sha256`); the URL inside the manifest uses `releases/latest/download`, so the
address never changes between releases.

- **Check.** Settings has a "Check for updates (installed <version>)" button, and the
  tray menu has the same entry. Both call `check_updates`, which does the network work on
  its own thread (a slow answer must not freeze the panel) and reports through message
  boxes. `update::check` fetches the manifest (64 KB cap), parses it (https only, 64 hex
  digits for the digest) and compares versions by dotted numbers, so a suffix after `-`
  or `+` never makes a version newer. A newer version asks once before anything is
  downloaded.
- **Download and verify.** WinHTTP (`WinHttpOpen`, automatic proxy setting, 10/10/20/20 s
  timeouts, redirects followed, 64 MB cap) fetches the installer into
  `%TEMP%\better-search-setup.exe`; `BCrypt` computes its SHA-256, and a mismatch deletes
  the file and refuses to install. Only then does `ShellExecuteExW runas` run the
  installer, and its exit code is checked.
- **Restart.** The installer cannot overwrite a running program's image, so it renames
  the old one aside (see Phase 5). The window keeps running the old code until
  `restart` closes it with `WM_RESTART` and starts the new build from the same path,
  hidden if the panel was hidden.
- **Only network access.** A plain HTTPS GET for a public file; nothing about this
  machine is sent. `BETTER_SEARCH_UPDATE_URL` overrides the manifest address for tests,
  and `http://` is accepted only for `localhost`.
- **Diagnostics.** `bs-window.exe --check-updates` prints the result and exits 0 (nothing
  newer), 2 (something newer) or 1 (the check failed).
- **Messages from the setup program.** `WM_QUIT_PANEL` (`WM_APP+9`) asks the panel to
  quit, which the uninstall uses before removing the files (see Phase 5).
- `tools/release.ps1` publishes: it refuses unless the version matches the workspace
  `Cargo.toml`, builds the release binaries and the setup program, writes `latest.txt`
  with the new installer's digest, and creates the release with `gh release create`
  (`-DryRun` stages the two files without publishing).

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
| Engine as a library (`bs-engine`), shells only print | Both the service and the console tool need the same behaviour; a log callback keeps the library testable and the service quiet |
| Service runs as LocalSystem | It needs MFT and journal access at boot, before any user logs in; the user installs once |
| Machine-wide snapshot in `%ProgramData%` with its own protected DACL | The service runs as SYSTEM, so the index must not be readable by users; a protected DACL does not pick up ProgramData's "users may read" entries |
| Move a planted data folder aside instead of fixing or deleting it | Any user can pre-create the folder; fixing a junction would change its target, fixing a user-owned folder leaves the owner able to undo it, and deleting could remove files behind a link |
| Pipe owned by Administrators, client checks the owner | While the service is stopped anyone can create the pipe name; the owner is the one property an ordinary user cannot fake, and the check costs one call per connection |
| Cancel a blocked journal read instead of a stop flag | A blocked `ReadFile` never checks a flag; `CancelSynchronousIo` makes a stop take milliseconds |
| "Loading" replies instead of refusing connections | The first full scan takes about a minute; a client should be told to wait, not fail |
| Named pipe with a message protocol, remote clients rejected | Local only (locked in), and message mode removes partial-read bugs from the first version |
| `INTERACTIVE` users get read/write but no `FILE_CREATE_PIPE_INSTANCE` | While the service runs, a user cannot add a fake instance and see another user's queries (the owner check covers the time it does not run) |
| Per-user privacy inside the search pass (a filter), not after it | Hidden entries must be missing from the match count too, otherwise the number is a lie |
| Privacy by profile folder, not by file ownership | Ownership is not printed in results and would need an ACL read per entry; the folder is the unit users think in |
| The caller's SID and profile path only, read at identification level | Reading a token is not acting as the user; identification level cannot do anything else |
| Rebuild the privacy map on a users epoch, not on every change | A rebuild costs 7 ms; the epoch separates "a file appeared in a profile" from "a profile moved" |
| Narrowing cap at 8192 names | Re-checking remembered names costs about 80 ns each, the SIMD scan about 5 ns per name: long lists are cheaper to search fresh |
| Do not narrow the privacy map by caller | One index serves all users; a per-user map would double the memory and the rebuild cost for a filter that already costs under a millisecond |
| Profiles from the registry, rechecked at most every 10 s before a search | Windows may keep a profile anywhere; reading about ten registry keys costs well under a millisecond, and a long-lived search window must not keep seeing a profile created after it connected |
| Service accounts (LocalSystem, LocalService, NetworkService) are not profiles | No person searches as them, and hiding their folders would only hide system files from administrators |
| Shared 64-connection limit, no per-user cap | The caller is known only after its first message, so a per-user cap cannot count silent connections; the worst case is a local slowdown, not exposed data |
| `bs query` in the existing console tool, not a new binary | Keeps one small tool; the pipe client is in `bs-pipe` so the UI can reuse it |
| One persistent window connection and a worker thread | Type-ahead narrowing works across keystrokes, and blocking pipe I/O cannot freeze input or painting |
| Virtual list and cached shell system icons | Only visible rows request text/icons; an extension-level cache avoids filesystem I/O and a per-result icon allocation |
| Per-user settings, not service configuration | Hotkey/startup need no admin rights; changing service-wide indexed drives requires a separate backend design |
| No fake skipped-folder count or drive filtering in the UI | The protocol cannot return skipped contents or search a subset of drives accurately; the user explicitly deferred the locked protocol/index changes |

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
- Snapshot at `%LOCALAPPDATA%\better_search\index.bin` for the console tool, format v3
  (bump `VERSION` when the format changes); `%ProgramData%\better_search\index.bin` with
  its protected SYSTEM + administrators DACL and owner Administrators for the service;
  a data folder that fails the checks in 5.7 is moved aside, never used.
- The service runtime: LocalSystem, started by the SCM, machine-wide data folder,
  log file at 1 MB with one old copy, `--console` for testing, "loading" replies while
  the index is not ready, save on stop and no save on shutdown.
- The pipe: `\\.\pipe\better_search`, message-mode protocol version 2 (bump `VERSION` on change), limit capped at
  1000 hits, remote clients rejected, `INTERACTIVE` users may read and write but not
  create instances, the pipe is owned by Administrators and clients refuse any other
  owner than SYSTEM or Administrators.
- Privacy: hide the contents of other users' profile folders (`<drive>\Users\<name>`
  and any other `ProfileImagePath` in the registry, service accounts excepted)
  from results **and** match counts, for administrators too; the profile folder itself
  stays visible only under its current name; `C:\Users\Public` is shared.
- UI requirements: tray icon, Alt+Space hotkey, right-edge hover zone with a slide-in panel.
- Decided for Phase 3 (see below): background service first; hide other users' private
  folders from each user; include removable non-NTFS drives (as the last step of
  Phase 3); exclude network drives.

---

## 8. Next phases

### Phase 3: background service (steps 1-5 done at `9c479c7`)

**Goal:** the index lives in a Windows service that starts with Windows. The search
window (Phase 4) talks to it and needs no admin rights, so there is no UAC prompt except
once at install.

**Status:** steps 1-5 are built and verified end to end, with an elevated temporary
service (`target\admin_run\run8.ps1` to `run11.ps1`), a non-elevated client and the
release binaries. Steps 6 (removable non-NTFS drives) and 7 (final measurements) are
**deferred until after Phase 4**, not dropped. The step text below is the plan as it
was written; where the result differed, the difference is noted.

**Audit after step 5.** A review of the Phase 3 code found these, all handled:

- A planted data folder (5.7) and a fake pipe while the service is stopped (5.8):
  fixed in `11970b2`.
- Profile folders outside `<drive>\Users` were not private (5.9), a renamed users
  folder did not change the users epoch (5.2), and two tests were missing
  (`Profiles::extended`, the last step of the users-epoch test): fixed in `8cb2f4c`.
- A suspected search slowdown since `913618b`: measured side by side, there is none
  (section 4).
- **Accepted, not changed:** the 64 connections are shared by all users, so one local
  user can use them up and make searches wait for others. A per-user cap would not
  help: the service learns who is calling only after the first message, so connections
  that never send one cannot be counted per user, and a local user can slow the machine
  in simpler ways anyway. The data is never exposed this way.

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
   - Logs to a small rotating file at 1 MB with one old copy, next to the snapshot
     (chosen over the event log: the per-drive scan lines and the skipped-entry summary
     stay readable).
   - A console mode (for example `bs-service --console`) for debugging without
     installing.
3. **Local connection (named pipe).**
   - Pipe name such as `\\.\pipe\better_search`.
   - Security descriptor: local interactive users only; remote access rejected
     (`PIPE_REJECT_REMOTE_CLIENTS`).
   - A small binary protocol: request = query text + result limit (+ options such as
     "include skipped folders" later); response = total match count + the best results
     (full path, is-folder flag, score). Results are capped so replies stay small.
   - Several clients at once; one thread per connection or overlapped I/O. Done with a
     blocking thread per connection, at most 64 connections; overlapped I/O can come
     later if many clients ever matter.
   - Target: well under 1 ms added to a search. **Measured: 0.1-0.2 ms**, connection
     0.2 ms.
4. **Per-user privacy.**
   - The service identifies the caller with `ImpersonateNamedPipeClient` /
     `GetNamedPipeClientProcessId` and the caller's token (user SID, profile path).
   - Each user's results leave out other users' profile folders (`<drive>\Users\<other>`).
     Administrators get the same filtering by default.
   - Possible later: also hide folders whose permissions deny that user (checking
     permissions per result with `AccessCheck`).
   - Built as described in 5.9: impersonation at **identification** level is enough (the
     plan did not say which level), the profile path comes from
     `ProfileList\<SID>\ProfileImagePath` in the registry, and a test on the real index
     hid 77 of 351,791 `e` matches (the machine's `Default` profile) from a normal user.
5. **Test client.** `bs query "text"` (or a small separate tool) sends a query through
   the pipe and prints the results, so everything can be tested without the UI.
   Built as `bs query` inside the existing tool, with `--bench` for round trip, service
   time and overhead, and `-n` for the limit.
6. **Implemented after Phase 4, pending physical hardware verification: removable
   non-NTFS drives (FAT, FAT32, exFAT: USB sticks, SD cards).**
   - `crates/service/src/removable.rs` checks `GetLogicalDrives` / `GetDriveTypeW`
     every two seconds and tracks drive letter plus volume serial. It walks each
     new drive with `FindFirstFileExW` (`FindExInfoBasic`, large fetch with a fallback
     for older drivers); `ReadDirectoryChangesW` watches the whole tree. A change
     triggers a fresh scan beside the published index; only a complete replacement
     is published. A failed or oversized scan drops the old index rather than
     returning a misleading partial count. Reparse points are not traversed.
   - Each volume has a separate, memory-only `Index`, with the existing ranked search
     and profile privacy rules. The service merges the best hits and sums visible
     match counts across NTFS and removable volumes without changing pipe v1, the
     locked NTFS index arrays, or snapshot v3. No removable entries are saved.
   - Trade-off: repeated changes on a busy removable drive cause repeated full walks
     instead of per-file updates. This keeps rename handling and notification
     overflow recovery simple and correct, but may use disk and CPU during those
     walks. Entries appear after the first scan, not while it is in progress. A
     two-million-entry cap bounds index memory and explicitly declines larger drives.
   - Fixture tests exercise a scan, rename/delete, real directory notifications on
     a local folder, drive-serial change/removal, and profile-filtered match counts.
     Actual FAT/exFAT insertion, modification and unplug cannot be claimed tested:
     there is no removable drive attached. Hardware testing remains pending.
     NTFS-formatted removable drives and network drives are excluded.
7. **Measured after Phase 4** on the real fixed NTFS drives: service memory,
   cold/warm start, search round trip through the pipe, and idle CPU. See the
   final measurement table in section 4. Removable media performance and real
   unplug behavior remain unmeasured until physical hardware is available.

**Testing the service during development:** installing a service changes the system,
so ask the user before registering it. Use a clearly named temporary service (for
example `better_search_dev`) created with `sc.exe create` from an elevated test script,
and delete it (`sc.exe stop` + `sc.exe delete`) at the end of every test run, until
Phase 5 provides a real installer. This pattern worked at `9c479c7` (scripts
`target\admin_run\run8.ps1` to `run11.ps1`): each script registered the temporary
service, ran the console tool and the service, exercised the pipe from the same elevated
session or from a separate non-elevated one, deleted the service and the data folder,
and wrote its results to text files. Two things to watch:

- Run one script at a time and wait for its UAC prompt: approving a later prompt starts
  the second copy while the first is still running, and the two fight over the service
  and the port-like pipe name.
- Check that the service and `C:\ProgramData\better_search` are gone (as the user asked)
  before the next run, and start from a clean state.

### Phase 4: search window (native window built; backend extensions deferred)

**Built at `c626267`:** `crates/ui` builds `bs-window.exe` and implements the
window, virtual results, icons, actions, tray, configurable hotkey, delayed hover
slide, DPI/theme handling and per-user settings described in 5.11. Built in the
requested order: pipe-backed window, tray/hotkey, then hover/settings. A live
non-elevated window queried the temporary elevated service on the real drives;
`target/admin_run/ui_client.txt` records the match count, rows and memory. The
service test script (`run_ui_service.ps1`) deleted `better_search_dev` and
`C:\ProgramData\better_search`; absence was checked afterwards. The unavailable
state, settings navigation, Esc, hotkey and slide were exercised without elevation
in `run_ui_smoke.ps1`; `run_ui_hidden.ps1` checked tray-first startup. Subsequent
`ui_denied.txt` and `ui_loading_ready.txt` verified a user-owned fake pipe is denied
and a real service progresses from loading to 1,674 matches and 200 virtual rows
(5,640,192 window private bytes). Context actions and theme changes are implemented
but remain untested end to end. The edge hover zone and its slide were removed by
user decision on 2026-10-05; the panel opens from Win+S, Alt+Space and the tray only.

**Deferred by user decision:** the skipped-folder result count and per-folder
overrides. Neither the pipe protocol nor index model may change for this increment.
The current index knows the skipped folder names, but not the names/counts under
them. A correct on-demand option needs a versioned request/reply, bounded subtree
indexing or traversal outside the keystroke path, per-user override state, live
change handling, snapshot compatibility, and the same privacy filtering for both
counts and hits. Scanning every skipped subtree on every keystroke would destroy
interactive speed; indexing it all by default would undo the 85% clutter saving.
Decide and approve that design before touching those locked pieces. The window
also shows the current fixed NTFS drives read-only: client-side filtering of only
200 returned hits would give wrong rankings and counts, while service-wide drive
changes would require administrator configuration and a rescan. A future accurate
per-user drive filter also needs a versioned request and a filtered search pass.

The following bullets are the original Phase 4 plan; the deferrals above take
precedence over its backend work:

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

### Phase 5: installer (built and tested end to end on the development machine)

**Built at `tools/installer`.** WiX, `dotnet` and MSI
packaging are not installed on the development machine, so Phase 5 is a small
self-contained Rust program, package `better-search-setup`, binary
`better-search-setup.exe`. `build.rs` and `include_bytes!` embed `bs-service.exe`,
`bs-window.exe` and `bs.exe` from `target\release` into the one output file, and refuse
to build if any is missing or not a Windows executable. The crate is separate from the
workspace (`[workspace]` in its `Cargo.toml`), so `cargo test --workspace` does not
cover it and its own `target\` is git-ignored.

Modes:

- No argument: install. `--inspect`: print versions, install folder and payload sizes
  without touching the machine (read-only; safe to run).
- `--install-elevated` / `--uninstall-elevated`: the internal steps an elevated copy
  runs. A non-elevated start re-launches itself through `ShellExecuteExW` `runas`, **waits
  for that copy to finish**, and then starts the search panel as the user. The elevated
  step re-checks the token is elevated and refuses otherwise.
- `--uninstall`: the Apps & Features entry runs this; it elevates the same way.

The program is built with `windows_subsystem = "windows"` and has no console. It was a
console program, which meant a double-click flashed a window, printed nothing the user
could read, and gave no sign of whether anything had happened. Everything the user needs
to see is now a message box (a failure, or the note that the panel could not be started),
and `--inspect` borrows the console it was started from (`AttachConsole` plus `CONOUT$`,
because a program without a console subsystem has no working standard output) and falls
back to a message box when there is none.

**Starting the panel.** The window is registered to start at sign-in, so a fresh install
used to leave nothing to press Alt+Space in until the next sign-in. The install path now
starts it: the ordinary double-click reaches `install_entry` unelevated, waits for the
elevated copy, and then starts `bs-window.exe` directly, so the panel runs with normal
user rights. A setup program that is itself elevated has no such parent, so it borrows the
shell's token (`GetShellWindow`, `OpenProcess`, `DuplicateTokenEx`,
`CreateProcessWithTokenW`); that call can be refused with ERROR_ACCESS_DENIED, which is
why the ordinary path avoids it. If neither works, a message box says so and names the
program to start by hand. A panel that is already running is left alone, so an upgrade
does not open a second one.

Install behaviour: requires the registry entries and the service to be absent, then copies
the three binaries plus
itself as the uninstaller, creates the `better_search` service (`SERVICE_AUTO_START`,
LocalSystem, own process) and starts it, waits for `SERVICE_RUNNING`, and writes an HKLM
`Run` entry (`bs-window.exe --hidden`) and the HKLM `...\Uninstall\better_search` keys
(DisplayName, version, publisher, location, UninstallString, NoModify, NoRepair). Every
step after creating the folder is inside a rollback: on error it stops and deletes the
service, removes the registry entries, deletes the written files and removes the folder.
The window auto-start is machine-wide because the service is, so the panel appears for
every user; the snapshot and pipe keep their own per-service protection.
An existing folder is normally refused, but one that holds **nothing but our own
leftovers** (the setup program, and `.old` images from an upgrade) is taken over after
those files are cleared, so a reinstall after an uninstall does not have to wait for a
restart. A folder with any other file or subfolder is still refused, unchanged.

Upgrade behaviour (added with the update check): no argument on a machine where the
installation is registered runs `upgrade` instead of `install`. It opens the service and
stops it, replaces the three programs and its own copy, refreshes the registry entries
(`Run`, Apps & Features version and strings), starts the service again and waits for
`SERVICE_RUNNING`. Files are written to a `.new` file and renamed over the target; when
that fails because the program is running, the old image is renamed to `.old` first and
the new file takes its place, which Windows allows because a running image keeps delete
sharing. A failure puts the original back, so an installed program is never left
missing. `remove_leftovers` deletes renamed-old images afterwards, best effort: the
service's copy goes immediately because the service is stopped, while a running window's
copy cannot be deleted at all (Windows denies deleting a running image) and goes at the
next upgrade or uninstall. The window keeps running
the old code until it restarts; settings, snapshot and log are untouched.

Verified twice on the development machine (2026-10-05) with the released installer while
both the service and the window were running: the service stopped and started again,
`bs-service.exe.old` and `bs-window.exe.old` appeared, the running 0.1.0 window kept
answering searches through the service during and after the upgrade, and a fresh window
started from the replaced file. A third run confirmed the cleanup: with no window
running from the renamed image, the leftover `.old` file was gone afterwards.

Uninstall behaviour: refuses unless the installer's own `InstallLocation` matches the
expected folder (so it never deletes a service someone else registered); a missing
service alone no longer stops it, because an earlier run may have removed the service and
then failed. It stops and deletes the service, removes the registry entries and the three
binaries, then asks (default **No**) whether to delete the saved `index.bin` and logs. It
removes only those named files and then the now-empty data folder; it never deletes
unknown files recursively. Unknown files in the install folder are also left alone, and
they keep the folder.

The panel is usually running during an uninstall, and Windows refuses to delete a running
image. The uninstall therefore first posts `WM_QUIT_PANEL` (`WM_APP+9`) to the search
window, which quits exactly as the tray's Quit does, so the image is released and deleted
outright. A panel that does not answer (an older version, or another user's session) is
handled by `delete_or_move_out`: the file is renamed into the temp folder (allowed for a
running image, and on the same volume) and that copy is deleted at the next reboot; if the
temp folder is on another volume the file is scheduled for deletion where it stands. The
uninstaller's own copy is the one file that always takes that route, because it cannot
delete itself, so the only trace an uninstall leaves is one temp file that Windows removes
at the next reboot. A file that can be neither deleted nor moved is reported with a
message asking for it to be closed, instead of a raw error.

Because the panel quits on request and an upgrade moves the renamed image out of the
folder, `%ProgramFiles%\better_search` is gone as soon as the uninstall finishes, with
nothing left inside it for a reboot to clear.

Verified on the development machine (2026-10-05) with release 0.2.4 and one elevated
script: it installed over a folder that held only a leftover `bs-window.exe.old` (adopted
and cleared), started the panel, upgraded over that running panel (the renamed image was
moved out of the folder during the upgrade, and the panel kept working), then uninstalled
while the panel ran. Afterwards the install folder, the service, the `Run` value, the
Apps & Features entry, `%ProgramData%\better_search` and `%LOCALAPPDATA%\better_search`
were all gone, and the only trace was the uninstaller's own copy in the temp folder,
scheduled for deletion at the next reboot.

A fresh install was then run the way a user runs it, by starting the setup program
unelevated and approving one prompt: it installed 0.2.4, started the service, started the
panel **unelevated** (the token reports `TokenIsElevated = 0`), and both Alt+Space and
Win+S brought the panel to the front afterwards. Before that run the panel had to be
started by hand, which is what prompted the change.

An earlier version left a `bs-window.exe.old` in the folder, because the uninstall's
leftover cleanup used a plain delete that Windows refuses for a running image; that is
what `clear_upgrade_leftovers` and `WM_QUIT_PANEL` fixed.

**End-to-end test on the development machine (2026-09-28).** With the user's approval
the draft was installed and uninstalled once, on the real C:, D:, E: drives, using the
ignored elevated scripts `target\admin_run\phase5_install.ps1`,
`phase5_window.ps1`/`phase5_window_ui.ps1` and `phase5_uninstall.ps1` (reports
`phase5_install.txt`, `phase5_window.txt`, `phase5_ui*.png`, `phase5_uninstall.txt`):

- Install produced one started `better_search` service (Automatic, LocalSystem,
  `"C:\Program Files\better_search\bs-service.exe"`), the four files in
  `%ProgramFiles%\better_search`, the `Run` value
  `"…\bs-window.exe" --hidden`, and the full Apps & Features uninstall record. It
  loaded the existing snapshot (609,218 entries, 18.8 MB index) in 249 ms.
- Unelevated `bs.exe query readme` returned 1,674 matches in ~11 ms round trip, and the
  installed window, launched unelevated, showed "better_search · 1674 matches" with
  `readme` results in its Name/Path columns.
- Uninstall removed the service (SCM 1060 afterwards), the `Run` value and the uninstall
  key, deleted the three binaries, and kept the snapshot and log when the prompt's
  default (No) was chosen. That version queued the uninstaller and its folder for
  reboot-time deletion, which left one `better-search-setup.exe` in the folder until the
  next reboot; the later `delete_or_move_out` change removes both at once instead (see
  the paragraph above).

Still open after the test:

- The delayed self-delete is confirmed only through the queued
  `PendingFileRenameOperations` entries, not by observing a reboot. Since the uninstall
  now moves a running copy into the temp folder first, the queued entry concerns a temp
  copy rather than a file in Program Files.
- The binary is **not code signed**, so SmartScreen will warn. Signing stays open as
  originally planned; the version and update check are now built (`9227386`, section
  5.12).
- The installer's own checks are eight tests in the crate: the bundled payloads are
  Windows executables, a file swap writes through and leaves no scratch files behind, a
  file held without delete sharing is refused without damage, renamed-old images from an
  earlier upgrade are cleared without touching the current program, only our own files
  count as leftovers, a folder of leftovers is adopted while any other file stops that, a
  free file is removed while a missing one needs no work, and a file that cannot be
  removed is reported rather than silently leaving the folder behind.
- The original Phase 5 plan is otherwise unchanged: one installer file, one UAC prompt,
  clean uninstall offering to keep or delete settings, and later code signing.

### Later ideas (not decided)

- Showing results during the very first scan.
- More filters (only folders). `ext:` and `in:` exist.

---

## 9. Working practices for this repository

- **Checks before every commit:**
  ```powershell
  cargo fmt --all --check
  cargo test --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo build --release
  ```
  126 workspace tests pass (index crate 37, query crate 35, engine crate 9,
  service crate 10, pipe crate 6, ntfs crate 3, cli crate 4, window crate 22).
  The release build produces `bs.exe`, `bs-service.exe` and `bs-window.exe`.
- **Installer crate checks** (it is outside the workspace): build the release binaries
  first, then `cargo fmt`, `cargo test`, and
  `cargo clippy --all-targets -- -D warnings` with
  `--manifest-path tools\installer\Cargo.toml`. Its `target\` folder is git-ignored and
  its eight tests never touch the machine. `--inspect` is safe to run unattended;
  install, upgrade and uninstall change the machine and must not be run without asking
  (the one approved end-to-end run is recorded in Phase 5).
- **Measuring on real drives:** the assistant's terminal is not elevated. Test scripts go
  in `D:\better_search\target\admin_run\` (ignored by git) and are run with
  `Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList
  '-ExecutionPolicy','Bypass','-File','<script>'`, writing results to text files that are
  read afterwards. The user approves each UAC prompt, one script at a time (see the
  Phase 3 note above). Latest scripts: `run14.ps1` (a temporary `ProfileList` entry
  outside `Users`, with non-elevated probes between flag files), `run13.ps1` (an old and
  the current `bs.exe` alternately; build the old one in a `git worktree` outside the
  repository and remove it afterwards), `run12.ps1` (planted data folders against the
  real service; it waits for `tests12_done.flag` so non-elevated probes can run while the
  service is up), `run11.ps1` (final `bs --bench`), `run10.ps1`
  (privacy and live tests through the service), `run9.ps1` (first service-only run),
  `run8.ps1` (scan plus bench); their `*.txt` outputs carry the numbers quoted in
  section 4.
- **Live tests must run outside `D:\better_search\target`:** that folder sits next to
  `Cargo.toml`, so the clutter rules skip it. Use a folder such as `D:\bsprobe_live` and
  delete it afterwards.
- **Window integration tests:** ignored scripts `target\admin_run\run_ui_smoke.ps1`,
  `run_ui_hidden.ps1`, `run_ui_service.ps1` and `run_ui_client.ps1` wrote
  `ui_smoke.txt`, `ui_hidden.txt`, `ui_service.txt` and `ui_client.txt`. One elevated
  service script at a time, with a non-elevated
  client while it waits. The scripts refuse pre-existing service/data state, and the
  elevated script deletes only the test service and the folder it created.
- **Shell:** Windows PowerShell 5.1. No `&&` / `||`; use `;` and `$LASTEXITCODE`. Run
  cargo through `cmd /c "... 2>&1"` so stderr output does not produce a false error
  exit code. Three PowerShell traps hit in practice: `r` is an alias for `Invoke-History`
  (a one-letter report function silently swallowed its output), PowerShell quoting
  mangles the `binPath= "..."` argument of `sc.exe create`, so pass the binary path
  unquoted or build the argument list as an array, and `-replace` with a `$1`/`${1}`
  group inside a double-quoted pattern plus a `,` in the replacement silently mangles
  Cargo.toml dependency lines (use `.Replace()` for version bumps).
- **Commits:** write the message to `.git\COMMIT_DRAFT.txt`, run
  `git -C D:\better_search commit -q -F .git\COMMIT_DRAFT.txt` in a separate step, then
  delete the draft. Messages end with
  `Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>`.
  Do not change the git identity.
- **Remote and releases:** `origin` is `https://github.com/devashish-guliya/better_search`
  (public). `main` is pushed there, and `tools\release.ps1` publishes the setup program
  and the manifest as a GitHub release. The manifest address is compiled into every
  release, so the repository name must not change without a plan for installed copies
  (GitHub redirects renamed repositories, but the address is still worth keeping).
- **Versioning:** one version for the whole project, `version` in the workspace
  `Cargo.toml`. The window compares it with the released manifest, and the installer
  crate (built outside the workspace, so it cannot inherit) must carry the same number;
  `tools\release.ps1` sets it and refuses to publish a mismatch. Every release therefore
  raises both numbers and gets its own commit, and `release` tags are `v<version>`.
- **Style:** match existing code; comments only where the reason is not obvious; plain,
  direct explanations for the user, including trade-offs, before big decisions.
