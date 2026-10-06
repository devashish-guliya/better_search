# better_search: hand-off for a new chat

Give this file to a new chat as context. It explains what better_search is, what has been
built, how each part works, why it was built that way, the current state of the
development machine, and how to keep working on it safely.

`docs/PROJECT.md` is the full project record (about 1,100 lines, with measurements,
the security audit and per-phase notes). This file is the condensed, self-contained
version. If the two disagree, check the code, then fix the doc that is wrong.

State at writing: branch `main`, released as `v0.2.8` (2026-10-06). 147 workspace tests
and 8 installer tests pass, and the code is on GitHub with nine releases published.
v0.2.8 removed the Win+S and typing-in-Start triggers by user decision (section 5.4). better_search 0.2.6 is
installed and running on this machine; see "Current installed state" below.

---

## 1. What better_search is

A very fast, very lean **file and folder name search for Windows**. It is now also a
replacement for **Windows Start search and Explorer search**.

- Finds files, folders, apps (including Microsoft Store apps), Windows Settings pages
  and power commands by name as you type, in a few milliseconds.
- Uses about 22 MB of RAM for the service and a few MB for the window, with zero idle CPU.
- Local only. Nothing about your files or this PC leaves the machine, and no query is
  ever logged. Names only, not file contents (by design). The single exception is the
  update check, which the user starts and which only fetches a public file (section 5.7).
- Opens from a tray icon and the **Alt+Space** hotkey. (Win+S and typing in Start used
  to open it too; both were removed by user decision after v0.2.7, section 5.4.)
- Target users are ordinary people with 1–2 TB drives, not only developers.

The user (Devashish Guliya) wants plain, direct explanations and minimal token use.
Ask before big or machine-wide decisions.

## 2. Development machine

- Windows 11 build 26200, Ryzen 5 3500U, Rust 1.98.1 (MSVC, edition 2024), Git 2.47.1.
- Drives: `C:` 238 GB SSD; `D:` and `E:` are two partitions of one 932 GB HDD. About
  4 million files in total; about 610 K are kept in the index after clutter skipping.
- Repository: `D:\better_search`, branch `main`, with `origin` =
  `https://github.com/devashish-guliya/better_search` (public). Releases are published
  from there with `tools\release.ps1`; installed copies read the manifest at
  `releases/latest/download/latest.txt`.
- Shell: **Windows PowerShell 5.1**. No `&&` or `||`; use `;` and `$LASTEXITCODE`.
  Cargo writes progress to stderr, so PowerShell may report exit code 1 on success.
  Check the "Finished" line, or run it through `cmd /c "cargo ... 2>&1"`.
- The assistant's terminal is **not elevated**. Anything that needs admin rights runs
  through `Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden ...`, and the
  user approves the UAC prompt.

### Current installed state (important)

- better_search **0.2.6 is installed and running** on this machine (the user installed the
  release built earlier on 2026-10-05). The service answers searches (about 15 MB working
  set, a 4 MB index for about 573,000 files) and **Windows search was turned off through
  the panel** (the `DisableSearch` policy is set and `windows_search_asked=true`). The
  installed copy is the released 0.2.6, not the working tree; upgrades go through the
  panel's Check for updates button or by running the new installer.
- The 0.2.5 install had earlier been removed with its own uninstaller, which was the last
  end-to-end test of the removal: the folder in `C:\Program Files`, the service, the `Run`
  value, the Apps & Features entry, `%ProgramData%\better_search` (index and log) and
  `%LOCALAPPDATA%\better_search` (settings and history) are all gone, and nothing of
  better_search is left in the registry or on disk. The uninstaller also returned Windows
  search to on, so the restore path was exercised for real.
- Releases so far: 0.2.0 (first public), 0.2.1 (leftover cleanup), 0.2.2 (clean
  uninstall), 0.2.3 (quit the panel so its image is released), 0.2.4 (start the panel
  after install), 0.2.5 (the offer quoted measured numbers, and turning Windows search off
  frees its index), 0.2.6 (drives are search results; the offer is short and plain, with
  no numbers, and No is the default answer), 0.2.7 (the settings page redesigned in the
  panel's design language), 0.2.8 (Win+S and typing-in-Start triggers removed; the
  panel opens from Alt+Space and the tray only). An installed copy upgrades in place through the panel's Check
  for updates button; `bs-window.exe --check-updates` prints whether a newer release
  exists.
- Settings for the window live at `%LOCALAPPDATA%\better_search\window.cfg`. Open
  history: `%LOCALAPPDATA%\better_search\history.tsv`. Service data (index and log):
  `%ProgramData%\better_search` (protected; only SYSTEM and Administrators can read it).

## 3. Architecture overview

A Cargo workspace with eight crates, plus the installer as a separate crate.

| Crate | Binary | Purpose |
|---|---|---|
| `crates/ntfs` (`bs-ntfs`) | | Reads NTFS: the whole file list from the MFT, plus the USN change journal |
| `crates/index` (`bs-index`) | | Compact in-memory index, clutter rules, location classes, snapshot format |
| `crates/query` (`bs-query`) | | Ranked, parallel, case-insensitive name search, filters (`ext:`, `in:`) |
| `crates/engine` (`bs-engine`) | | Keeps the index loaded, current and saved; per-user privacy map |
| `crates/pipe` (`bs-pipe`) | | Named-pipe message format and the client |
| `crates/service` (`bs-service`) | `bs-service.exe` | Windows service (LocalSystem) that owns the index and answers searches |
| `crates/cli` (`bs-cli`) | `bs.exe` | Console tool: local index and search, `bs query` through the service, benchmarks |
| `crates/ui` (`bs-window`) | `bs-window.exe` | Unelevated native Win32 search window, tray, hotkeys, hooks |
| `tools/installer` | `better-search-setup.exe` | One-file installer and uninstaller, kept outside the workspace |

The data flow looks like this:

```
NTFS MFT + USN journal ──> bs-service.exe (LocalSystem, owns the index)
                                │  \\.\pipe\better_search (message mode, local only)
                                ▼
                         bs-window.exe (per user, unelevated)  /  bs.exe query
```

**Dependencies are deliberately few:** `windows-sys` (raw Win32), `hashbrown`, `zstd`,
`memchr`, `rayon`. The release profile uses `opt-level=3`, LTO, one codegen unit,
`panic=abort` and a stripped binary. A `release-small` profile (`opt-level="z"`) exists
for shipping.

**Why:** Rust with raw Win32 gives the smallest memory use and binary, with no runtime
(.NET, Electron and WebView were rejected). Reading the MFT takes seconds where walking
folders takes minutes. A service means the user approves admin rights only once, at
install.

## 4. Backend in detail

### 4.1 Reading drives (`bs-ntfs`)

- **Full list:** `FSCTL_ENUM_USN_DATA` on `\\.\C:` returns every record (record number,
  parent, name, attributes) straight from the MFT. It needs admin rights. The volume
  root is record 5.
- **Live changes:** `FSCTL_READ_USN_JOURNAL` from a saved position. The read blocks
  until something changes, so idle CPU is zero.
- **Missing journals** are created automatically (`FSCTL_CREATE_USN_JOURNAL`, 32 MB max,
  8 MB growth). The user approved this; without a journal, every start would need a
  full rescan.

### 4.2 The index (`bs-index`, `crates/index/src/lib.rs`)

- **Parallel arrays**, one slot per entry: `name_ids: Vec<u32>`, `parents: Vec<u32>`,
  `flags: Vec<u8>` (DIR, HIDDEN, location class bits, DELETED, SKIPPED). **Full paths
  are never stored.** They are rebuilt from parent links, and only for shown results.
  This costs about 32 bytes per entry in total.
- **Name table:** unique names, lowercased, back to back in one UTF-8 buffer, plus an
  uppercase bitmask. One SIMD scan covers every name, and the original case can still
  be shown. A hashbrown interner shares duplicate names; only 47% of names are unique.
- **Record lookup (`RecordMap`):** a sorted record-number list per volume (binary
  search), plus a small `recent` map for entries created since the last compaction.
  This is cheaper than a full hash map.
- **Location classes** (used for ranking and hiding): `UserContent` (Desktop,
  Documents, Downloads, Pictures, Videos, Music, OneDrive, Dropbox, `source`, `repos`,
  `projects`...), `StartMenu`, `AppFiles` (a program's own files, recognised by
  folder shape), `Noisy` (Windows, ProgramData, AppData, `$` folders, `node_modules`,
  toolchains...), and `Normal`.
- **Live updates:** a new entry is added **only if its parent folder is indexed** and
  not skipped. That one rule also keeps the contents of clutter folders out. Deletes
  only set a flag. `end_batch()` re-checks project rules, removes orphans, fixes
  locations and bumps `generation`, which resets search caches.
- **Compaction** happens at about 2% garbage. It builds a clean copy under a read lock,
  so it never blocks searches, then swaps it in.
- **Users epoch:** counts changes that can move entries between user profiles. It
  decides when the privacy map must be rebuilt.

### 4.3 Clutter skipping

About 85% of the files on a PC are ones nobody searches for by name. better_search
skips the **contents** of such folders but keeps the folder itself searchable.

- Drive-root `$` folders and System Volume Information.
- Tool-only names skipped anywhere: `.git`, `node_modules`, `__pycache__`, `.venv`,
  `.gradle`, `.cargo`, `.nuget` and similar.
- Common names skipped only in Noisy areas: WinSxS, Temp, Logs, caches and similar.
- Names skipped only when a project file confirms them: `target` next to `Cargo.toml`,
  `bin`/`obj` next to a .NET project, `build`/`dist` next to `package.json`, Unity and
  Unreal folders, and so on.
- Folders that label themselves: `CACHEDIR.TAG`, `pyvenv.cfg`, `CMakeCache.txt`.
- **Any rule change must bump `SKIP_RULES_VERSION`** (currently 8), which forces one
  rescan of about 35–60 s.
- Rejected approaches: reading `.gitignore` (it hides files people do search for) and
  guessing by file count (that would hide photo libraries).
- Side effect: `D:\better_search\target` itself is skipped, so live tests must use a
  folder elsewhere, such as `D:\bsprobe_live`.

### 4.4 Search (`bs-query`, `crates/query/src/lib.rs`)

- Terms are separated by whitespace, and every term must match.
- **Pass 1, per unique name:** a SIMD `memchr` scan for the longest term, then scoring.
  The best match type wins: exact > prefix > word start > substring. Exact stems with a
  launchable extension (`.exe`, `.lnk`, ...) reach the top tier. Installers, `.dll`,
  `.log` and similar files are pushed down. Documents and media are pushed up, and
  source and generated files are pushed down.
- **Folder words:** with 2–8 terms, a term missing from the name may match a parent
  folder name. It then counts 20 points.
- **Pass 2, per entry:** location bonuses (UserContent +25, StartMenu shortcuts +50,
  AppFiles −30, Noisy −45). Matches in system and app folders are **hidden by default**
  and counted as "N more in system and app folders" (Ctrl+H shows them).
- **Per-thread top-N heaps** mean the full match list is never built.
- **Acronyms:** for a short single word with few matches, `vsc` finds
  `Visual Studio Code.lnk`.
- **Type-ahead narrowing (`Session`):** when the new query extends the old one, only
  earlier matches are re-checked, capped at 8,192 names (above that, a fresh SIMD scan
  is cheaper).
- **Filters:**
  - `ext:pdf,docx` filters by extension. On its own, it lists every file of that type.
  - `in:"C:\Some Folder"` keeps only entries below that folder. Quotes are optional
    without spaces, `/` is accepted, and the last one wins. A search with it takes
    about 11 ms through the service.
- **Drives are results:** searching `c`, `c:` (any letter) finds the volume root, which
  pass 2 used to drop. A root must carry every term in its own name (it has no folders
  above it for folder words) and is never below its own `in:` scope. The window shows the
  volume's label ("Data (D:)", else "Local disk (C:)"), the shell's drive icon and a
  "Local disk" second line; Enter opens the drive in Explorer.
- Typical speed through the service: 1–3 ms per keystroke, and 9–11 ms for a single
  letter.

### 4.5 Snapshot (`crates/index/src/snapshot.rs`)

- Format v3: a `BSIX` header plus one zstd level-3 stream with delta coding. It is
  about 4.5 MB and saves in about 0.3–0.5 s. It is written to a temp file, then renamed.
- It is validated fully on load. A rescan happens when the skip rules version, the set
  of drives, a volume serial or a journal ID changed, when the saved position fell
  outside the journal, or when the file is damaged.
- After loading, changes are replayed from the journal (a catch-up of a few ms).

### 4.6 Engine (`crates/engine`)

- One watcher thread per drive, a saver thread (hourly if anything changed, plus on a
  normal stop, never at machine shutdown), and an `RwLock` index with a maintenance
  mutex so long read-locked jobs never queue a writer in front of searches.
- **Idle trim:** after 60 s without a search, `EmptyWorkingSet` is called.
- Stop is fast because `CancelSynchronousIo` interrupts blocked journal reads, retried
  every 5 ms.

### 4.7 Service (`crates/service`)

- `bs-service.exe` runs under the SCM as LocalSystem. `--console` runs it in a
  terminal for testing.
- **Data folder:** `%ProgramData%\better_search`, with a protected DACL (SYSTEM and
  Administrators only, owner Administrators). A planted junction or a user-owned
  folder is **moved aside, never used** (`security.rs`).
- **Log:** `service.log`, rotated at 1 MB with one old copy. Queries are never logged.
- While loading, it answers `Loading` instead of refusing connections.
- **Removable drives:** FAT-family drives get memory-only indexes (`removable.rs`).
  Network drives are excluded.

### 4.8 Named pipe (`crates/pipe`)

- `\\.\pipe\better_search`, message mode, remote clients rejected, at most 64
  connections in total.
- **Security descriptor:** network logons are denied. SYSTEM and Administrators get
  full access. INTERACTIVE users get read and write but cannot create new instances.
  The pipe is owned by Administrators, and **the client refuses a pipe owned by anyone
  else**, which protects against a fake server while the service is stopped.
- **Request:** `version · kind · limit u16 (cap 1000) · options (bit 0 = include system
  folders) · UTF-8 query`.
- **Reply:** `status · total · hidden · µs · hits[score, flags, path]`. Every message
  carries a version, and a mismatch is rejected.

### 4.9 Per-user privacy (`crates/engine/src/profiles.rs`)

- Other users' profile folders are hidden from both the results **and the match
  counts**, for administrators too.
- The caller is identified once per connection with `ImpersonateNamedPipeClient` at
  identification level (SID plus profile path).
- **Privacy map:** one byte per entry (a profile slot), rebuilt only when the users
  epoch changes (7 ms) and extended otherwise. Profiles are re-read from the registry
  at most every 10 s, so a profile outside `Users` is private too.

### 4.10 Console tool (`bs.exe`)

- `bs` builds its own elevated local index. `bs --walk <folder>` and
  `bs --synthetic <N>` cover tests and benchmarks.
- `bs query <text>` searches through the service, and `bs query --bench` measures it.
- `bs report <index.bin>` shows where entries live.

## 5. The search window (`crates/ui`) in detail

This is where most of the recent work went. It is pure Win32 through `windows-sys`.
Per-monitor v2 DPI awareness and common controls v6 come from an embedded manifest
(`build.rs`, `bs-window.manifest`).

| File | What it does |
|---|---|
| `main.rs` (about 3,100 lines) | The `App` struct, window procedure, layout, tray, hotkeys, scope chip, message handlers, opening results, `main()` arguments |
| `settings_page.rs` | The settings page's layout and painting: rows, cards, switches, chevrons, notes, the hotkey band and its capture state |
| `search.rs` | Worker thread with a persistent `bs_pipe::Client`; coalesces edits and drops stale replies with a serial number |
| `draw.rs` | Visual system: spacing and type scales, Segoe UI Variable, Fluent icon glyphs, light/dark colour roles, accent colour, the safe `text()`/`text_width()` helpers |
| `rows.rs` | Row model: `Kind` (App, Folder, Document, Media, Settings, Other...), sections and grouping, match highlighting |
| `thumbs.rs` | Background STA thread: real app icons and file thumbnails through `IShellItemImageFactory` |
| `apps.rs` | Microsoft Store and MSIX apps read from `shell:AppsFolder`, refreshed after 2 minutes |
| `commands.rs` | About 50 `ms-settings:` pages with keywords, plus power commands |
| `frecency.rs` | Open history (`history.tsv`), seeded from Windows Recent |
| `settings.rs` | Loads and saves `window.cfg` |
| `winkey.rs` | Foreground watcher: ends `SearchHost.exe` while Windows search is off |
| `winsearch.rs` | Turns Windows search off or on (elevated), and frees its index when off |
| `stats.rs` | One read-only sizes request, off the UI thread, so the panel can quote real numbers |
| `update.rs` | Update check, download, SHA-256 check and running the installer |

### 5.1 Look and behaviour

- Default geometry: a square panel whose side is 70% of the work area height, centred
  vertically and flush with the right screen edge (`panel_rect`), used at startup.
- Three bands: a rounded search field (glyph, placeholder, accent underline), the
  results, and a footer with the count and a hint.
- Results are an owner-data, owner-drawn ListView with two-line rows: an icon, the
  name with matched letters in semibold, and a second line (the folder, shortened in
  the middle, or a description such as "App", "Windows tool" or "Web link").
- Rows are grouped under section headings (Apps, Settings, Folders, Documents, Photos
  videos and music, Other files), in the order of each section's best hit. Headings
  cannot be selected.
- The hovered or selected row gets rounded fills, an accent pill, and "Show in folder"
  and "Copy path" buttons.
- Keys: Enter opens, Ctrl+Enter shows the item in Explorer, arrows move, Esc hides,
  and Ctrl+H toggles system and app folders. A context menu offers Open, Show in folder
  and Copy path.
- Shortcuts are shown without `.lnk`/`.url`/`.appref-ms`. Duplicate Start Menu
  shortcuts collapse into one.
- Up to 200 service hits are shown. Store apps and Settings rows are merged in and
  are **not** cut by that limit (a retain loop).

### 5.2 Ranking added in the window

- **Frecency:** each open adds weight, and the weight halves every 14 days. The bonus
  is `min(60, 16·ln(1+w))`, followed by a stable re-sort. At most 2,000 paths are kept.
- **Store apps** score 180 for an exact name, 144 for a prefix, 127 for a word start and
  100 for a substring.
- **Settings and commands** score like Store apps minus 12. A keyword prefix of 3 or
  more letters scores 120 − 12 (`KEYWORD`, `BELOW_APPS`).

### 5.3 Settings pages and power commands (`commands.rs`)

- About 50 `ms-settings:` pages plus `windowsdefender:`, each with search keywords.
  They use the Settings app icon (`Kind::Settings`, `ICON_SOURCE`).
- `command:` entries: Shut down, Restart and Sign out ask first in a MessageBox. Sleep
  uses `SetSuspendState`, and Lock uses `LockWorkStation`.

### 5.4 Win+S and typing in Start (removed after v0.2.7)

By user decision (2026-10-06) better_search no longer touches Win+S or typing in
Start: the low-level keyboard hook, the Explorer scope chip, and the `win_s=` setting
are gone, and the panel opens from Alt+Space, the tray, or launching it again. Win+S
does what Windows does with it, which never starts indexing: the WSearch indexer runs,
or is off, regardless, and opening the search UI cannot re-enable a service the panel
turned off. `in:"<folder>"` still works when typed by hand. What remains of
`winkey.rs` is the `EVENT_SYSTEM_FOREGROUND` watcher: while Windows search is off, a
`SearchHost.exe` that still comes to the front is ended at once (it no longer opens
the panel).

### 5.6 Turning Windows search off (`winsearch.rs`)

- `is_off()` reads the HKLM policy
  `SOFTWARE\Policies\Microsoft\Windows\Windows Search\DisableSearch`.
- `request(hwnd, off)` re-runs `bs-window.exe` elevated through `runas` with
  `--windows-search on|off` and waits. `main()` handles that argument before anything
  else and exits with the elevated copy's code.
- `apply(off)` returns that exit code, which is the only way the elevated half can report:
  `0` off and its index files removed, `1` off but the index files stayed, `2` on and its
  settings restored, `3` on but Windows would not take its indexer back, `4` the policy
  could not be written. `read_code` turns the code into the `Outcome` the panel shows, and
  a test pins every mapping so a wrong code cannot be read as a success.
- Turning it off: the policy, then `stop_indexer()` (start type `SERVICE_DISABLED`, stop,
  and **wait up to 10 s** for `SERVICE_STOPPED`, because the index files stay locked while
  the process runs), then `taskkill SearchHost.exe` (killing it alone is not enough, it
  restarts within seconds), then `clear_index()`.
- **`clear_index()` is what frees the disk space**, not the policy: it removes the contents
  of `C:\ProgramData\Microsoft\Search\Data` and keeps the folders, so Windows' own
  permissions stay. Windows rebuilds the index by itself if search runs again, so nothing
  is lost. Measured on this PC: 10.9 MB and 17 files before, 0 MB and 0 files after, and
  Windows rebuilt 8.3 MB within 30 s of being turned back on.
- Turning it on: delete the policy, `ChangeServiceConfigW` to `SERVICE_AUTO_START`,
  `ChangeServiceConfig2W` for delayed auto start (as Windows ships it), and
  `StartServiceW`. It does **not** wait for the indexer: Windows brings it up in its own
  time, the panel is blocked while this runs, and a wait long enough to see it would freeze
  the window. What is reported is the setting that makes search work again.
- **First-run offer:** the first time the panel opens (or is launched visible) while
  Windows search is still on, `offer_windows_search` shows one `MessageBoxW` Yes/No;
  `windows_search_asked=true` in `window.cfg` records the answer, and Yes runs the elevated
  switch. If Windows search is already off, the flag is set without a dialog.
- The offer is **three plain sentences, no numbers**: the question ("Do you want to turn
  off the Windows indexing of your files?"), what indexing is (Windows keeps a list of
  your files so its search can find them; better_search already keeps its own, smaller and
  faster list), and the honest cost with the undo (programs that search inside documents
  such as Outlook would search more slowly; one click in Settings turns indexing back on).
  It shows when the panel is visible and idle; if the user is typing, the ask is
  postponed until the sizes reading for Settings arrives. **"No" is the default button**
  (`MB_DEFBUTTON2`): the change is machine-wide, so not choosing is not choosing yes. A
  test pins that the wording has no numbers in it.
- One-click row: the label says what the press will do ("Turn Windows search off (asks
  for admin)" or "Turn Windows search back on (asks for admin)"), depending on the current
  state, with a chevron instead of a button. A press calls `winsearch::request` at once and
  then `winkey::set_search_off`; nothing about it goes through Save. A press that freed the
  disk space shows "Windows search is off, and its index files are gone"; one that did not
  says so and names the folder.
- **Watcher:** an `EVENT_SYSTEM_FOREGROUND` WinEvent hook (`foreground_changed`). If
  `SearchHost.exe` comes to the front while search is off, the hook ends it.

### 5.6a Showing what each one costs (`stats.rs`, `bs_pipe::StatsReply`)

- The service answers a read-only sizes request: `version u8 · kind u8 (2)` in, and
  `version u8 · status u8` plus nine `u64` sizes out (`u64::MAX` for one that is not known
  yet, which is not the same as zero). It needs no viewer lookup: it says nothing about any
  file, so a window that may not search may still ask what things cost.
- The service measures Windows search's footprint, because it is the only part of
  better_search with the rights to read `C:\ProgramData\Microsoft\Search`: `winsearch.rs` in
  the service walks that folder and sums the **working set** of every `SearchIndexer.exe`
  (the number Task Manager's Memory column shows, so a user can check it). A figure it
  cannot get is reported as unknown, never as a zero.
- The panel shows it in Settings as "Using 25 MB of memory and keeping a 4 MB index for
  574,000 files and folders", measured once per start (`start_sizes` in `run()`) and again
  after a Windows search switch. The panel's own memory is not in the reply: the service
  cannot measure the panel, and the text does not need it.
- Measured on this PC (2026-10-05, 574 K entries): our service 20-21 MB private, 25-29 MB
  working set after a scan, trimmed to 0.8 MB when idle, index 4.2 MB on disk and 17.6 MB
  in memory. Windows search: `SearchIndexer` 8.6 MB private and 19.3 MB working set while
  idle with a fresh index, 29 MB while rebuilding, and 34.3 MB of index files for a full
  index (10-11.5 MB after a rebuild). The disk comparison is index against index, which is
  what grows with the number of files.

### 5.10 The settings page (`settings_page.rs`, redesigned 2026-10-05)

- The page is **fully parent-drawn** in the search panel's design language; there are no
  native controls on it any more. `build()` lays out `Line`s top to bottom (headings,
  toggle rows, action rows, wrapped notes), measures wrapped notes with the caller's
  device context, and records the hotkey band's rect, the back button and the title.
- `paint()` draws rounded cards (edge outside, surface inside), the section headings in
  the small caps font, Windows 11-style switches (accent pill with a white knob when on),
  chevrons on action rows, the back arrow (`E72B`) and title, and the hotkey band drawn
  like the search field. Everything shifts up by the scroll offset; lines that have
  scrolled away are not drawn.
- **Instant apply:** a toggle click or Enter applies at once (`apply_toggle`) and saves;
  there is no Save button and nothing to lose.
- **Hotkey capture is hand-written.** Clicking the band (or pressing Enter on it) starts
  the capture: the band shows "Press the keys…", the keys held so far ("Ctrl + Shift"),
  or a hint ("Use Ctrl, Alt, Shift or Win with another key." / "That hotkey is already in
  use. Choose another."), with the search field's accent underline while capturing. The
  main window's `WM_KEYDOWN` and `WM_SYSKEYDOWN` feed `capture_key`; Escape cancels;
  modifiers only update the prompt. A final key applies at once
  (`apply_captured_hotkey`): valid combos need a modifier or an F1-F24 alone
  (`valid_combo`); a failed `RegisterHotKey` puts the old combination back and explains
  in the band. `WM_SYSCHAR` and `WM_SYSCOMMAND SC_KEYMENU` are swallowed on the page, or
  Alt+Space opens the window's system menu instead of finishing the capture.
  `hotkey_name` writes the combination the way Windows does ("Ctrl + Shift + K"), and
  Win-key combinations now work (the old `msctls_hotkey32` control could not capture
  them and ignored the theme).
- **Keyboard and wheel:** Up/Down move the cursor over rows (headings and notes are
  skipped, and the view scrolls just enough to keep the row on screen), Enter or Space
  activates, Escape goes back to the search field; the wheel scrolls by a row.
- Three tests pin the layout: lines stack without gaps or overlap, every row is
  reachable by hit-testing, and a switch holds its state in its row; two more pin
  `hotkey_name` and `valid_combo`.


### 5.7 Updates (`update.rs`) and releases

- Releases live at `github.com/devashish-guliya/better_search`. Each one carries
  `better-search-setup.exe` plus `latest.txt` (`version`, `url`, `sha256`), and the URL
  inside the manifest uses `releases/latest/download`, so it never changes.
- **Check for updates** in Settings (and in the tray menu) runs `check_updates`: the
  fetch happens on its own thread, and every answer comes back as a message box. A newer
  version asks once before downloading anything. The download is HTTPS through WinHTTP
  with the system proxy, its SHA-256 is checked with `BCrypt`, a mismatch is deleted and
  refused, and only then does `ShellExecuteExW runas` run the installer (one UAC prompt),
  checking its exit code. Afterwards the window offers to restart into the new build
  (`WM_RESTART`, which closes the window and starts the new program hidden or visible).
- This is the only network access better_search makes. Nothing about the machine is
  sent. `BETTER_SEARCH_UPDATE_URL` overrides the manifest address for tests (http is
  allowed for `localhost` only), and `bs-window.exe --check-updates` prints the result
  and exits 0 / 2 / 1 for nothing newer / newer / failed.
- Publishing: raise the version in the workspace `Cargo.toml`, then
  `.\tools\release.ps1 -Version 0.3.0` (add `-DryRun` to stage the files only). The
  script checks the version, builds, writes the digest into `latest.txt` and runs
  `gh release create`.

### 5.8 The typing crash (fixed)

- **Symptom:** the window crashed when typing fast. The event log showed USER32 at
  offset `0x24a82` (codes `c0000005` and `c000041d`); `cdb` resolved it to
  `USER32!DrawTextExWorker`.
- **Cause:** when the bold run of a name was cut to nothing, `DrawTextW` received a
  count of 0 with a dangling pointer from an empty `Vec`.
- **Fix:** `draw::text` returns early for empty text and always passes a NUL-terminated
  buffer, and `text_width("")` returns 0. The test `drawing_empty_text_is_safe` covers
  it.

### 5.9 Other window features

- **Tray:** Open, Settings, Pause (pauses window queries only), Check for updates and Quit.
- **Settings gear:** a glyph button at the right end of the search field opens the same
  Settings page (`WM_LBUTTONDOWN` hit test on `App::gear`), so Settings needs no tray trip.
- **One panel per session:** `main()` looks for the window class first; a second launch
  posts `WM_SHOW_PANEL` (`WM_APP+7`) to the running window and exits. `WM_RESTART`
  (`WM_APP+8`) is the update flow's "start the new build", and `WM_QUIT_PANEL`
  (`WM_APP+9`) is the setup program's "exit before I remove the files".
- **Indexing wait:** while the service answers `Loading`, the status line says
  "Indexing your drives; results appear when the first scan finishes".
- **Hotkey:** default Alt+Space, changeable in Settings. If another program owns it,
  the tray still works.
- **No edge hover:** the screen-edge pop-in was removed by user decision (2026-10-05); Win+S, Alt+Space and the tray open the panel.
- **Start with Windows:** a per-user HKCU Run entry, off unless enabled. The installer
  adds a machine-wide HKLM Run entry.
- The theme follows the Windows light or dark app setting.
- The settings page explains that the clutter safety net (counts of skipped files,
  per-folder unskip) is **deferred by user decision**: the index has no entries inside
  skipped folders, so honest counts are impossible without more backend state.

## 6. Installer (`tools/installer`)

- A Rust program that embeds the three release binaries with `include_bytes!`;
  `build.rs` refuses to build if they are missing. It shows one UAC prompt, because it
  re-launches itself with `runas`, and it **waits for that elevated copy to finish**, so
  the outcome it reports is real.
- It is built with `windows_subsystem = "windows"` and has no console: everything the user
  sees is a message box. `--inspect` borrows the console it was started from and falls back
  to a message box when there is none.
- **Install:**
  - copies the files into `%ProgramFiles%\better_search`;
  - creates and starts the `better_search` service (auto start, LocalSystem);
  - adds an HKLM Run entry for `bs-window.exe --hidden` and an Apps & Features
    uninstall entry;
  - **starts the search panel**, unelevated, so Alt+Space and Win+S work at once instead of
    after the next sign-in (the unelevated half of the double-click does this directly; an
    already-elevated setup program borrows the shell's token, which can be refused, and then
    says so in a message box);
  - rolls back fully if any step fails;
  - refuses a folder that exists, **unless** it holds nothing but our own leftovers (the
    setup program and `.old` images), which it clears and takes over, so a reinstall
    straight after an uninstall works without a restart.
- **Uninstall:** checks `InstallLocation` first (a missing service alone no longer stops
  it), removes the service, registry entries and binaries, asks whether to delete the
  index and logs (default **No**), and removes the folder. The panel is usually running,
  and Windows refuses to delete a running image, so the uninstall first posts
  `WM_QUIT_PANEL` (`WM_APP+9`) to the search window, which quits like the tray's Quit.
  Anything still locked after that (a panel from an older version, another user's panel,
  and always the uninstaller's own copy) is renamed into the temp folder and deleted at
  the next reboot, so `%ProgramFiles%\better_search` is gone immediately with nothing left
  inside it. Unknown files are never deleted and keep the folder.
- **Upgrade (added with the update check):** running the setup program on a machine where
  the install is registered upgrades in place instead of refusing. It stops the service,
  replaces the three programs and its own copy, refreshes the registry entries, and
  starts the service again. Each file is written as `.new` and renamed over the target;
  when the program is running (rename refused) the old image is renamed to `.old` first,
  which Windows allows for a running image, and the new file takes its name. The renamed
  copy is moved out of the folder during the upgrade (`clear_upgrade_leftovers`, using the
  same treatment as an uninstall, because the panel keeps running from it). The running
  window keeps the old code
  until it restarts. A failure restores the original. Verified on this machine with the
  released installer
  while the service and the window were running: the service stopped and started, the
  `.old` copies appeared and were then cleared, and the old window kept answering searches
  during and after the upgrade.
- `--inspect` is read-only and safe to run. **Do not run install, upgrade or uninstall
  without asking the user.** The binary is not code signed, so SmartScreen will warn.
- **It starts the panel** when the install finishes, so Alt+Space and Win+S work at once
  instead of after the next sign-in. Which way depends on who the setup program is: the
  ordinary double-click path reaches `start_panel` unelevated, after the elevated half has
  finished, and starts `bs-window.exe` directly, so the panel gets normal user rights. A
  setup program that is itself elevated (right-click, "Run as administrator") borrows the
  shell's token with `CreateProcessWithTokenW`, and if Windows refuses that (it did:
  `ERROR_ACCESS_DENIED`, 5) it asks the running shell to open the program
  (`explorer.exe <path>`), which starts it at the user's own level. Nothing is reported as
  started until `FindWindowW` finds the panel's window, up to 10 s. Both paths were
  verified on this machine.
- **It reports in message boxes and has no console.** A console window that flashed and
  closed made a finished install and a failed one look identical. The elevated half is
  waited for, so the outcome is real, and a dismissed UAC prompt says so instead of
  exiting silently.
- **An uninstall turns Windows search back on** if better_search had turned it off
  (policy, indexer start type, start), and removes this account's settings and history
  along with the index and logs when the user says Yes. Without the first, removing the
  app would leave search disabled with nothing left to undo it; without the second, a
  fresh install would never offer again because `windows_search_asked` would still be
  `true`.
- Separate crate: run its checks with `--manifest-path tools\installer\Cargo.toml`
  (eight tests: payloads are executables, a file swap writes through cleanly, a file
  without delete sharing is refused without damage, renamed-old images are cleared, only
  our own files count as leftovers, a folder of leftovers is adopted while any other file
  stops that, a free file is removed while a missing one needs no work, and a file that
  cannot be removed is reported).

## 7. Key decisions (and why)

- **Native Rust/Win32 only** gives minimal RAM and binary size. Locked in.
- **MFT plus USN journal**, not folder walking or `ReadDirectoryChangesW`: fast, exact,
  zero idle CPU, and it catches up on changes made while closed.
- **Parallel arrays, no stored paths, and a shared name table** give the minimum bytes
  per entry. Locked in.
- **Skip clutter contents but keep the folders:** 85% fewer entries, and nothing
  becomes invisible.
- **Service as LocalSystem, with a protected ProgramData folder**, because MFT access
  is needed at boot and other users must not read the index.
- **Privacy filter inside the search pass**, so counts never leak hidden entries.
- **Hide system and app-folder matches by default:** a measurement showed 94% of index
  entries live there.
- **Window-side ranking extras** (frecency, Store apps, Settings) leave the pipe and
  index unchanged.
- **The low-level hook runs on its own thread**, because Windows silently removes slow
  hooks.
- **Close Start with Escape:** Start cannot be pushed out of the foreground any other
  way that was tried.
- **Windows search off through the policy plus the service**, because killing
  `SearchHost.exe` alone is undone within seconds.

### Locked in (do not change without discussing with the user)

- Search stays name-only, local-only and Windows-only, with NTFS as the primary source.
- Rust, `windows-sys` and native UI.
- The index data model, journal following, auto-created 32 MB journals, the clutter
  rules (bump `SKIP_RULES_VERSION`), the "parent must be indexed" rule, compaction at
  about 2%, hourly saves plus a save on normal quit, and the 60 s idle trim.
- Snapshot format v3 (bump `VERSION` on change) and its locations and DACLs.
- Pipe name, security, owner check and the 1,000-hit cap.
- Privacy rules (other profiles hidden, `Public` shared).
- UI: tray, Alt+Space, Win+S.

## 8. How to work on this repository

### Checks before every commit

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
```

147 tests: index 37, query 35 plus 3 drive-root tests, window 33, service 14,
engine 9, pipe 9, cli 4, ntfs 3.
The installer crate has 8 more (its own `--manifest-path`).

### Updating the installed window after a change

```powershell
cargo build --release
Get-Process bs-window -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '-NoProfile','-Command','Copy-Item -Force D:\better_search\target\release\bs-window.exe ''C:\Program Files\better_search\'''
Invoke-CimMethod Win32_Process Create -Arguments @{CommandLine='"C:\Program Files\better_search\bs-window.exe" --hidden'}
```

`Invoke-CimMethod` starts the window detached from the agent's shell. To update the
service, stop the `better_search` service elevated, copy `bs-service.exe`, then start
it again.

### Testing the window automatically

- Send keys with `keybd_event` from a small `Add-Type` C# class, then read the state.
  Find the `BetterSearchWindow` top-level window **with `EnumWindows` plus
  `GetClassName`**, then read its `Edit` child with `WM_GETTEXT`. **`FindWindow`
  returned the wrong or no window here, so do not use it.**
- **Never inject Escape in tests.** It reached the Factory desktop app and opened its
  "Stop agent?" dialog. Hide the window by bringing `Shell_TrayWnd` to the front
  instead.
- Screenshots: `System.Drawing` `CopyFromScreen`, saved under `target\`, which is
  ignored by git.
- Elevated measurement scripts live in `target\admin_run\` (ignored), and the user
  approves each UAC prompt.
- `target\ui_probe.ps1` (ignored) drives and reads the real UI from outside: `-Mode list`
  (visible top-level windows), `dialogs` (every visible `#32770` with all its child texts,
  which is how a message box's wording and buttons are read), `dialog` / `click`
  (screenshot a dialog, or click a named button in it), `panel`, `show-panel`, `settings`.
- PowerShell does not set `$LASTEXITCODE` for a windows-subsystem program, and does not
  wait for it. To read an exit code from `bs-window.exe`, use
  `Start-Process -Wait -PassThru` and read `.ExitCode`.

### Tooling gotchas

- The Edit tool often reports "file modified externally" after `cargo fmt` or a
  PowerShell write. Re-read the file, then edit.
- Define missing Win32 constants locally (for example `EM_GETSEL`) instead of adding
  `windows-sys` features, unless the feature is needed anyway.
- PowerShell: `r` is an alias for `Invoke-History`, and `sc.exe create binPath= "..."`
  quoting gets mangled. A C# class named `K` with a method called `Main` fails to
  compile in `Add-Type` (it is treated as an entry point).

### Commits and releases

- Match the existing style (a short imperative summary).
- End the message with
  `Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>`.
- Never change the git identity.
- `origin` is the public GitHub repository `devashish-guliya/better_search`; `main` is
  pushed there. To publish a release, raise the version in the workspace `Cargo.toml`
  and run `.\tools\release.ps1 -Version <version>`; the script also sets the installer
  crate's version and refuses a mismatch. The manifest address compiled into
  the window uses the repository name, so do not rename the repository casually.
- Update `README.md` and `docs/PROJECT.md` (and this file) when behaviour changes.

### Style

- Match the surrounding code.
- Add comments only for non-obvious reasons.
- Write plain, short explanations for the user, and lay out the trade-offs before big
  choices.

## 9. Commit history (newest first)

| Commit | What it added |
|---|---|
| `daf246e` | Installer version 0.2.8 |
| `11d0fda` | Version 0.2.8 |
| `a691f26` | Win+S and typing-in-Start triggers removed; the panel opens from Alt+Space and the tray only |
| `00f43f0` | Installer version 0.2.7 |
| `b995be5` | Settings redesign released; version 0.2.7 |
| `152b75e` | Record the settings redesign in the project docs |
| `bc202d8` | Settings redesigned in the search panel's design language: parent-drawn page, switches, hand-written hotkey capture |
| `e310679` | Search finds the drives by letter or `c:`; the first-run offer is short and plain; version 0.2.6 |
| `3021d1a` | The offer quotes measured numbers, and turning Windows search off frees its index; version 0.2.5 |
| `16640cb` | Start the panel after install, wait for elevation, report in message boxes; version 0.2.4 |
| `2c447e0` | Quit the panel on uninstall so its image is released; version 0.2.3 |
| `9c91fdf` | Leave Program Files clean on uninstall; adopt a folder of our own leftovers (v0.2.2) |
| `ddbf755` | Clear renamed-old images after an upgrade; version 0.2.1 |
| `85669b7` | Run cargo and gh through `cmd.exe` in the release script |
| `9227386` | GitHub releases, in-place installer upgrades, and a Check for updates button |
| `02d0b37` | Record the release and update work in the project docs |
| `d9b81c6` | First-run offer to turn Windows search off, with the reasoning |
| `c79bc7a` | Settings gear, one-click Windows search switch, indexing wait, one panel per session |
| `21b0540` | Drop the legacy hover/left keys from the settings test |
| `e65a578` | Square panel (side = 70% of screen height), edge-hover pop-in removed |
| `9a79804` | Default the window to a 9:16 right-edge panel at 70% of the screen |
| `7b7810a` | Bring the project record up to date with the window work since Phase 5 |
| `438c7da` | Typing crash fix (empty `DrawTextW`), Windows search off/on plus the SearchHost watcher, typing in Start opens better_search |
| `0310085` | Win+S hook, `in:` folder scope from Explorer, scope chip, Settings pages and power commands |
| `3fef8b5` | UI redesign: two-line owner-drawn rows, sections, highlights, `draw.rs` visual system |
| `890f744` | Microsoft Store apps via `shell:AppsFolder`; WindowsApps counted as app files |
| `334c87b` | App display names, real icons and thumbnails (`thumbs.rs`) |
| `a66bb7a` | Folder-word matching, `ext:` filter, ranking fixes |
| `841e4dc` | Rank by kind (apps, documents, media, folders, code); exact names win |
| `78efd7a` | Hide system and app-folder matches by default; seed history from Recent |
| `03a2ecb` | Start Menu and installer ranking tuning |
| `7663a83` | Word-initial (acronym) matching |
| `58e16ad` | Frecency: rank what the user opens higher |
| earlier | Installer (Phase 5), live stats (later removed), removable FAT drives, Phase 4 window, security audit fixes, Phase 3 service/pipe/privacy, clutter rules, snapshot, Phase 1 index (`d0c31f2`) |

Run `git log --oneline` for the full list of 39 commits.

## 10. Open items and ideas

- **Clutter safety net** (skipped-file counts, per-folder unskip): deferred by the
  user. It needs backend state.
- **Choosing which drives the service indexes** in the UI: deferred. It needs a
  protocol and backend design.
- **Installer:** code signing (the update check and in-place upgrade are now built).
- Showing results during the very first scan (about a minute): not decided.
- More filters (for example folders only). `ext:` and `in:` already exist.
- Watch in daily use:
  - `sc stop WSearch` taking a long time.
