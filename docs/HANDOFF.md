# better_search: hand-off for a new chat

Give this file to a new chat as context. It explains what better_search is, what has been
built, how each part works, why it was built that way, the current state of the
development machine, and how to keep working on it safely.

`docs/PROJECT.md` is the full project record (about 1,100 lines, with measurements,
the security audit and per-phase notes). This file is the condensed, self-contained
version. If the two disagree, check the code, then fix the doc that is wrong.

State at writing: branch `main`, released as `v0.2.1` (2026-10-05). The tree is clean,
126 workspace tests and 4 installer tests pass, and the code is on GitHub with the
first two releases published.

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
- Opens from a tray icon, the **Alt+Space** hotkey, **Win+S**, or by **typing while
  Start is open**.
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

- better_search is **installed** in `C:\Program Files\better_search` (`bs-service.exe`,
  `bs-window.exe`, `bs.exe`, `better-search-setup.exe`), at version 0.2.1. The
  `better_search` service runs
  as LocalSystem and starts automatically. The window auto-starts with `--hidden` from an
  HKLM `Run` entry.
- Version 0.2.0 was the first published release and 0.2.1 is the newest; the installed
  copy has been upgraded in place with the released installer, both by hand and through
  the panel's Check for updates button. `bs-window.exe --check-updates` prints whether a
  newer release exists.
- **Windows search is turned OFF on this PC.** The `DisableSearch=1` policy is set, the
  `WSearch` service is stopped and disabled, and `SearchHost.exe` is closed. To turn it
  back on, uncheck the Settings box or run `bs-window.exe --windows-search on` elevated.
- Settings for the window: `%LOCALAPPDATA%\better_search\window.cfg`. Open history:
  `%LOCALAPPDATA%\better_search\history.tsv`. Service data (index and log):
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
| `main.rs` (about 2,550 lines) | The `App` struct, window procedure, layout, settings page, tray, hotkey, scope chip, message handlers, opening results, `main()` arguments |
| `search.rs` | Worker thread with a persistent `bs_pipe::Client`; coalesces edits and drops stale replies with a serial number |
| `draw.rs` | Visual system: spacing and type scales, Segoe UI Variable, Fluent icon glyphs, light/dark colour roles, accent colour, the safe `text()`/`text_width()` helpers |
| `rows.rs` | Row model: `Kind` (App, Folder, Document, Media, Settings, Other...), sections and grouping, match highlighting |
| `thumbs.rs` | Background STA thread: real app icons and file thumbnails through `IShellItemImageFactory` |
| `apps.rs` | Microsoft Store and MSIX apps read from `shell:AppsFolder`, refreshed after 2 minutes |
| `commands.rs` | About 50 `ms-settings:` pages with keywords, plus power commands |
| `frecency.rs` | Open history (`history.tsv`), seeded from Windows Recent |
| `settings.rs` | Loads and saves `window.cfg` |
| `winkey.rs` | Low-level keyboard hook (Win+S, typing in Start), foreground watcher for SearchHost, Explorer folder lookup |
| `winsearch.rs` | Turns Windows search off or on (elevated) |
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

### 5.4 Win+S and Explorer folder scope (`winkey.rs`)

- A `WH_KEYBOARD_LL` hook runs **on its own thread**, because Windows drops slow
  low-level hooks.
- Win+S is swallowed, but Win+Shift+S is not, so the screenshot tool still works. The
  hook taps the unassigned key `0xE8` so releasing Win does not open Start, then posts
  `WM_WIN_S` (`WM_APP+5`). Injected keys pass through, so remapping tools work too.
- If an Explorer window is in front, its active tab's folder is read through
  hand-written COM vtables: `IShellWindows` → `IServiceProvider` → `IShellBrowser` →
  `IFolderView` → `IPersistFolder2`. The window then shows an **"In <folder>" chip**,
  and requests are sent as `in:"<folder>" <text>`, without Store apps or Settings.
  Backspace at the start of the field removes the chip. Alt+Space always opens
  unscoped.
- Foreground rights are borrowed with `AttachThreadInput` to the front window's thread
  (`open_from_hook` in `main.rs`).
- Setting: `win_s=` in `window.cfg`, default on. The checkbox reads "Win+S and typing
  in Start open better_search".

### 5.5 Typing in Start opens better_search

- While `StartMenuExperienceHost.exe` is in front, the hook swallows letters and digits
  (Shift and Caps Lock are handled in `typed_char`) and posts `WM_START_TYPED`
  (`WM_APP+6`, the character in `wParam`).
- The foreground check runs at each key press (`start_in_front`) and is cached per
  foreground window (`LAST_FRONT`, `LAST_FRONT_IS_START`). The foreground event arrives
  too late for the first letters.
- **Start refuses to give up the foreground.** Alt-tap tricks failed. The window
  therefore checks `is_start(GetForegroundWindow())`, injects **Escape** to close Start
  (`close_start`), waits up to 500 ms until Start has left the front (so the Escape
  cannot reach our own window, which would hide it), then taps `0xE8` and takes the
  foreground.
- Letters that arrive within 1.5 s (`start_typed_at`) are appended to the **edit's
  current text** (`App::edit_text()`). `last_text` lags because search is debounced,
  and using it once lost a letter.
- Verified: typing "notepad" and "calc" in Start arrived in full in the better_search
  field.

### 5.6 Turning Windows search off (`winsearch.rs`)

- `is_off()` reads the HKLM policy
  `SOFTWARE\Policies\Microsoft\Windows\Windows Search\DisableSearch`.
- `request(hwnd, off)` re-runs `bs-window.exe` elevated through `runas` with
  `--windows-search on|off` and waits. `main()` handles that argument before anything
  else.
- `apply()` sets or deletes the policy, runs `sc config` and `sc stop WSearch`
  (disabled, or delayed-auto plus start), and ends `SearchHost.exe`. Killing it alone
  is not enough, because it restarts within seconds.
- **First-run offer:** the first time the panel opens (or is launched visible) while
  Windows search is still on, `offer_windows_search` shows one `MessageBoxW` Yes/No
  explaining the saving; `windows_search_asked=true` in `window.cfg` records the answer,
  and Yes runs the elevated switch. If Windows search is already off, the flag is set
  without a dialog.
- One-click button: the label says what the press will do ("Turn Windows search off (asks
  for admin)" or "Turn Windows search back on (asks for admin)"), depending on the current
  state (control id `SETTINGS_WINDOWS_SEARCH` 321, `controls[2]`, the first control under
  the hotkey field). A press calls `winsearch::request` at once and then
  `winkey::set_search_off`; nothing about it goes through Save. The double-height control
  index is 8.
- **Watcher:** an `EVENT_SYSTEM_FOREGROUND` WinEvent hook (`foreground_changed`). If
  `SearchHost.exe` comes to the front while search is off, the hook ends it and opens
  better_search (`WM_WIN_S`).
- The first elevated run took over 120 s, probably in `sc stop`, but it completed.

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
  posts `WM_SHOW_PANEL` (`WM_APP+7`) to the running window and exits.
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
  re-launches itself with `runas`.
- **Install:**
  - copies the files into `%ProgramFiles%\better_search`;
  - creates and starts the `better_search` service (auto start, LocalSystem);
  - adds an HKLM Run entry for `bs-window.exe --hidden` and an Apps & Features
    uninstall entry;
  - rolls back fully if any step fails.
- **Uninstall:** checks `InstallLocation` first, removes the service, registry entries
  and binaries, asks whether to delete the index and logs (default **No**), and queues
  its own deletion for the next reboot.
- **Upgrade (added with the update check):** running the setup program on a machine where
  the install is registered upgrades in place instead of refusing. It stops the service,
  replaces the three programs and its own copy, refreshes the registry entries, and
  starts the service again. Each file is written as `.new` and renamed over the target;
  when the program is running (rename refused) the old image is renamed to `.old` first,
  which Windows allows for a running image, and the new file takes its name. The renamed
  copy is deleted afterwards when it can be (the service's always, because it is stopped
  for the swap; a running window's image cannot be deleted at all, so it goes at the next
  upgrade or uninstall). The running window keeps the old code
  until it restarts. A failure restores the original, and a folder that exists without a
  registration is still refused. Verified on this machine with the released installer
  while the service and the window were running: the service stopped and started, the
  `.old` copies appeared, and the old window kept answering searches during and after the
  upgrade. A later run cleared a leftover `.old` file once no window was running from it.
- `--inspect` is read-only and safe to run. **Do not run install, upgrade or uninstall
  without asking the user.** The binary is not code signed, so SmartScreen will warn.
- Separate crate: run its checks with `--manifest-path tools\installer\Cargo.toml`
  (four tests: payloads are executables, a file swap writes through cleanly, a file
  without delete sharing is refused without damage, renamed-old images are cleared).

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

126 tests: index 37, query 35, engine 9, service 10, pipe 6, ntfs 3, cli 4, window 22.
The installer crate has 3 more (its own `--manifest-path`).

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

Run `git log --oneline` for the full list of 36 commits.

## 10. Open items and ideas

- **Clutter safety net** (skipped-file counts, per-folder unskip): deferred by the
  user. It needs backend state.
- **Choosing which drives the service indexes** in the UI: deferred. It needs a
  protocol and backend design.
- **Installer:** code signing (the update check and in-place upgrade are now built).
- Showing results during the very first scan (about a minute): not decided.
- More filters (for example folders only). `ext:` and `in:` already exist.
- Watch in daily use:
  - typing in Start on slower machines (the 500 ms wait for Start to close);
  - the Explorer scope with virtual folders (they give no scope);
  - `sc stop WSearch` taking a long time.
