# better_search

Very fast file and folder name search for Windows.

The index is kept in memory by a background service. It reads fixed NTFS drives
directly, follows the NTFS change journal and saves a compressed snapshot. Removable
FAT/FAT32/exFAT drives get separate, memory-only indexes while attached. The console
client and a native Win32 search window search through a named pipe without admin rights.

The full design record (decisions, what is settled, and the plan for the next phases) is
in [docs/PROJECT.md](docs/PROJECT.md).

## Layout

| Crate | Purpose |
|---|---|
| `crates/ntfs` | Reads every file record of an NTFS volume with `FSCTL_ENUM_USN_DATA`, and the change journal |
| `crates/index` | Compact index: shared UTF-8 name table, parent links, location tags, clutter rules, snapshot |
| `crates/query` | Parallel ranked search that keeps only the best results; per-result filter hook |
| `crates/engine` | Keeps the index loaded, current and saved; per-user profile map for privacy |
| `crates/pipe` | Message format of the service's named pipe, and a client for it |
| `crates/service` | `bs-service.exe`: the background service that owns the index |
| `crates/cli` | `bs`: console tool that indexes and searches locally, and `bs query` through the service |
| `crates/ui` | `bs-window.exe`: native search panel, virtual results, tray, hotkey, hover zone, per-user settings |

## Usage

```powershell
cargo build --release

# Launch the unelevated search window. Keep the service running for results.
.\target\release\bs-window.exe

# Search through the running service (no admin rights).
.\target\release\bs.exe query readme
.\target\release\bs.exe query --bench

# Index all fixed NTFS drives locally. Needs a terminal opened with
# "Run as administrator".
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

The window starts with Alt+Space (if Windows has not reserved it) and a short hover at
the right screen edge. Double-click its tray icon to open it; right-click for Open,
Settings, Pause or Quit. Pause stops window searches, not the background index. Type to
search, use the arrow keys, Enter to open, Ctrl+Enter to select the result in Explorer,
or right-click a result for Open, Open folder and Copy path. Esc hides the window.
The status line distinguishes a stopped service, an index still loading and access
denied. It shows the highlighted result's full path. The window retries while a
service is starting.

The window is plain Win32 through `windows-sys` (no C#, no web view, no extra runtime;
the binary is about 260 KB). An embedded app manifest activates the modern common
controls and per-monitor DPI awareness, and the panel draws its own flat light/dark
theme: rounded Windows 11 corners, a themed title bar, rounded outlines around the
search box and the result list, and a Segoe UI Variable font.

Results include matching file and folder **names**, with their full location in the
Path column. Enter opens the selected item, including an app's `.exe` or a shortcut
when that file is indexed. This is not a general app catalog or a search of text
inside full folder paths. Contents of skipped clutter folders are absent by design.

Settings let you change the hotkey and edge side, disable edge hover, and explicitly
enable per-user start at sign-in (HKCU only). Window settings are saved at
`%LOCALAPPDATA%\better_search\window.cfg`. At sign-in it starts hidden in the tray;
run `bs-window.exe --hidden` to do that manually. The drives shown in Settings are the fixed
NTFS drives eligible for the service; changing the indexed drives is deferred because
the current service and pipe have no per-user drive filter. The skipped-folder result
count and per-folder overrides are also deferred: the v1 index holds no skipped-folder
contents, so any count here would be misleading. The search pipe protocol and index
model remain unchanged.

The service checks for removable FAT/FAT32/exFAT drives every two seconds. It walks a
new drive once, watches file and folder changes, and rebuilds that drive's index after
a change. Results appear after the scan completes and disappear on removal. A scan that
fails (including an oversized drive with more than two million entries) yields no
partial results. Removable indexes are not saved to disk, so they scan again on each
service start. NTFS-formatted removable drives and network drives are not covered yet.
The removable behavior has been tested with folder fixtures, not physical USB hardware.

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

The console tool keeps its snapshot at `%LOCALAPPDATA%\better_search\index.bin`. It is
thrown away and the drives are read again if a drive's serial number or journal changed,
if too many changes were missed while the tool was closed, or if the clutter setting
changed.

Drives without a change journal get one (32 MB, the size Windows uses for the system
drive), so their snapshot stays valid too.

## Installer

`tools\installer` builds a small self-contained setup program (`better-search-setup.exe`)
that embeds the three release binaries, so no WiX or .NET tooling is needed. Install,
search and uninstall were tested end to end on the development machine; the binary is
still **not code signed**, so SmartScreen warns.

```powershell
# Build the binaries first, then the setup program.
cargo build --release
cargo build --release --manifest-path tools\installer\Cargo.toml

# Read-only: print the install folder and bundled payload sizes.
.\tools\installer\target\release\better-search-setup.exe --inspect

# Install (asks for one UAC prompt; writes to Program Files and registers the service).
.\tools\installer\target\release\better-search-setup.exe
```

What the draft does if it is run:

- Copies `bs-service.exe`, `bs-window.exe` and `bs.exe` into
  `%ProgramFiles%\better_search`, and copies itself there as the uninstaller.
- Registers and starts `better_search` as an auto-start service running as LocalSystem,
  and adds a machine-wide `Run` entry that starts `bs-window.exe --hidden` at sign-in.
- Refuses to run if the service, install folder or registry entries already exist, and
  rolls back files, service and registry entries if any step fails.
- Registers an entry in Apps & Features. Its Uninstall string runs
  `better-search-setup.exe --uninstall`, which stops and deletes the service, removes
  the files and registry entries, and asks (default No) before deleting the saved
  snapshot and logs. It refuses to uninstall unless the install registration matches.
  The uninstaller removes itself and its folder on the next reboot.

Verified end to end on this machine: install started the service and the unelevated
window showed "1674 matches" for `readme`, and uninstall removed the service, the
files and the registry entries while keeping the snapshot. The one remaining file, the
uninstaller itself, needs `Program Files` write access to delete and is queued for
removal on the next reboot. The binary is not code signed, so SmartScreen will warn.

Registering the service can also still be done by hand (`sc.exe create`) or with a
temporary development test.

## The service

`bs-service.exe` is meant to run as a Windows service (started by the Service Control
Manager as LocalSystem at boot), so the index is always ready and no search needs admin
rights.

```powershell
# Run it in a terminal instead of installing it, for testing.
.\target\release\bs-service.exe --console
```

- Data folder: `%ProgramData%\better_search` (snapshot `index.bin`, `service.log`). The
  service sets a protected DACL that gives SYSTEM and administrators full control, so a
  normal user cannot read the index (it contains every file name on the machine). Any
  user can create folders in `%ProgramData%`, so on every start the service checks the
  folder first: a junction or link, a folder owned by someone other than SYSTEM or
  Administrators, or links inside it make the service rename it to
  `better_search.untrusted-...` and create a fresh one. It never follows or reuses such
  a folder.
- The log holds one line per event, rotates at 1 MB, and never records queries.
- While the index loads or is scanned for the first time, a search answers
  "the index is still loading" instead of failing.
- A normal stop saves first (a few hundred milliseconds); a machine shutdown does not
  save, and the change journal replays whatever was missed.
- Registering the service changes the machine, so it is done by an installer
  (Phase 5) or by hand: `sc.exe create better_search binPath= <full path to
  bs-service.exe>` and `sc.exe start better_search`.

### The pipe

The service listens on `\\.\pipe\better_search` (message mode, one message per request
and reply, remote clients rejected). Local interactive users may connect and search but
cannot create their own instance of that pipe name. The pipe is owned by Administrators,
and the client refuses a pipe with any other owner before it sends a query, so a
program that takes the name while the service is stopped gets no queries. Overhead is
0.1-0.2 ms per query.

### Privacy between users

The service reads the caller's SID and profile path while impersonating them at
identification level (reading the token, not acting as the user). Results and match
counts leave out the contents of other users' profile folders (`<drive>\Users\<name>`,
and any profile Windows keeps elsewhere, read from the registry), for administrators
too. The folder itself stays visible, `C:\Users\Public` is shared,
and a folder that happens to match the caller's profile name on another drive stays
visible. The per-entry privacy map costs about 0.6 MB and is rebuilt only when a profile
folder appears, disappears or moves.

### Clutter folders

By default the contents of folders no ordinary user searches are left out. The folders
themselves stay searchable. Three kinds of rules decide what is clutter, without any setup:

- **Tool-only names**, skipped anywhere: `node_modules`, `.git`, `__pycache__`, `.venv`,
  `.gradle`, `.next`, `.dart_tool`, `.terraform` and similar.
- **Common names confirmed by a project file** next to them: `target` next to `Cargo.toml`,
  `bin` and `obj` next to a `.csproj` or `.sln`, `build` or `dist` next to `package.json`,
  Unity's `Library` next to `Assets` and `ProjectSettings`, and so on. A `target` folder
  anywhere else is indexed normally.
- **Folders that label themselves**: a folder containing `CACHEDIR.TAG` (a standard cache
  marker), `pyvenv.cfg` (a Python virtual environment) or `CMakeCache.txt` (a CMake build).

Inside system or app-data areas, caches, temp folders, logs and Windows component stores
such as `WinSxS` are skipped too. If a project file or label appears after the folder, the
folder's contents are removed at that point. New files are only added when their folder is in the index, so files
created inside skipped folders stay out, and a folder moved into a skipped folder leaves
the index together with its contents. The full list is in
`crates/index/src/lib.rs` (`SKIP_ANYWHERE`, `SKIP_IN_NOISY`, `SKIP_IN_PROJECT`).

After 60 seconds without a search the index is allowed to leave RAM; the first search after
that reads it back in.

## Checks

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
