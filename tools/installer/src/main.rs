//! Self-contained Windows setup. Building this binary is read-only; install and
//! uninstall are explicit, elevated operations and are never run by the build.
//! Running it on a machine where better_search is installed upgrades in place.
//!
//! The program has no console of its own: a console window that flashed and closed gave
//! no idea whether anything had happened, so everything the user needs to see is a message
//! box, and a fresh install starts the search panel so the hotkey works at once.
#![windows_subsystem = "windows"]

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SERVICE_DOES_NOT_EXIST, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DUPLICATE, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation, TokenPrimary,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, WriteConsoleW};
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, REG_DWORD, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW,
    RegGetValueW, RegSetKeyValueW,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE,
    SC_STATUS_PROCESS_INFO, SERVICE_ALL_ACCESS, SERVICE_AUTO_START, SERVICE_CONTROL_STOP,
    SERVICE_ERROR_NORMAL, SERVICE_RUNNING, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOPPED,
    SERVICE_WIN32_OWN_PROCESS, StartServiceW,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessWithTokenW, GetCurrentProcess, INFINITE, LOGON_WITH_PROFILE, OpenProcess,
    OpenProcessToken, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    SHGetKnownFolderPath, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetShellWindow, GetWindowThreadProcessId, MB_DEFBUTTON2, MB_ICONERROR,
    MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MessageBoxW, PostMessageW, SW_SHOWNORMAL,
};

/// The error `ShellExecuteW` reports when the user dismisses the elevation prompt.
const ERROR_CANCELLED: i32 = 1223;

const SERVICE: &str = "better_search";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\better_search";
const FILES: [(&str, &[u8]); 3] = [
    (
        "bs-service.exe",
        include_bytes!("../../../target/release/bs-service.exe"),
    ),
    (
        "bs-window.exe",
        include_bytes!("../../../target/release/bs-window.exe"),
    ),
    ("bs.exe", include_bytes!("../../../target/release/bs.exe")),
];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn known_folder(id: &windows_sys::core::GUID) -> io::Result<PathBuf> {
    let mut raw = null_mut();
    // SAFETY: `raw` receives CoTaskMem-allocated NUL-terminated UTF-16.
    let status = unsafe { SHGetKnownFolderPath(id, 0, null_mut(), &mut raw) };
    if status < 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    // SAFETY: successful call returns a NUL-terminated string.
    let result = unsafe {
        let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
        let value = PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(
            raw, len,
        )));
        CoTaskMemFree(raw.cast());
        value
    };
    Ok(result)
}

fn install_dir() -> io::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_ProgramFiles)?.join("better_search"))
}

fn data_dir() -> io::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_ProgramData)?.join("better_search"))
}

fn elevated() -> io::Result<bool> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the process handle is a pseudo handle; `token` receives an owned handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut read = 0u32;
    // SAFETY: the output buffer is the declared size; the token is closed below.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut read,
        )
    };
    let error = io::Error::last_os_error();
    unsafe { CloseHandle(token) };
    if ok == 0 {
        Err(error)
    } else {
        Ok(elevation.TokenIsElevated != 0)
    }
}

/// Shows a message box. This program has no console of its own, so a message box is the
/// only way the user can see anything it has to say.
fn note(title: &str, text: &str, icon: u32) {
    // SAFETY: both strings are NUL-terminated, and the box belongs to no window.
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(text).as_ptr(),
            wide(title).as_ptr(),
            MB_OK | icon,
        )
    };
}

/// Re-runs this program elevated and waits for it, so the elevated half is the only thing
/// that reports an outcome. A dismissed elevation prompt is not a failure to report: the
/// user chose not to continue, and a message box would only nag.
fn request_elevation(mode: &str) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let file = wide_path(&exe);
    let verb = wide("runas");
    let args = wide(mode);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: args.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // SAFETY: the struct is initialised with its size and NUL-terminated strings.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_CANCELLED) {
            return Ok(());
        }
        return Err(err);
    }
    if !info.hProcess.is_null() {
        // SAFETY: the handle is owned by this call, waited on and then closed once.
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            CloseHandle(info.hProcess);
        }
    }
    Ok(())
}

/// Starts the search panel so Alt+Space and Win+S work straight after installing, instead
/// of waiting for the next sign-in. A panel that is already running is left alone.
///
/// The panel must not carry this program's administrator rights, and which way to start it
/// depends on who this process is: the ordinary double-click path reaches here unelevated,
/// after the elevated half has finished, and can start the panel directly. Only a setup
/// program that is itself elevated has to borrow the shell's token, and that can be refused.
fn start_panel(dir: &Path) {
    let result = if elevated().unwrap_or(false) {
        launch_panel_with_shell_token(dir)
    } else {
        launch_panel_here(dir)
    };
    if let Err(err) = result {
        note(
            "better_search",
            &format!(
                "better_search is installed, but the search panel could not be started \
                 ({err}).\n\nStart it yourself with:\n{}\n\nIt also starts by itself at \
                 your next sign-in.",
                dir.join("bs-window.exe").display()
            ),
            MB_ICONINFORMATION,
        );
    }
}

/// True when a panel is already running, so it must not be started twice.
fn panel_running() -> bool {
    let class = wide("BetterSearchWindow");
    // SAFETY: NUL-terminated class name; a null title matches any window of that class.
    !unsafe { FindWindowW(class.as_ptr(), null()) }.is_null()
}

/// Starts the panel as this process's own user, which is what the unelevated setup path
/// does. The panel inherits the normal user rights, so no token work is needed.
fn launch_panel_here(dir: &Path) -> io::Result<()> {
    if panel_running() {
        return Ok(());
    }
    std::process::Command::new(dir.join("bs-window.exe"))
        .spawn()
        .map(|_| ())
}

/// Starts the panel with the signed-in user's token, for the case where the setup program
/// itself is elevated. `CreateProcessWithTokenW` can refuse a shell token with
/// ERROR_ACCESS_DENIED, which is why the ordinary path above avoids it.
fn launch_panel_with_shell_token(dir: &Path) -> io::Result<()> {
    if panel_running() {
        return Ok(());
    }
    let shell = unsafe { GetShellWindow() };
    if shell.is_null() {
        return Err(io::Error::other("no signed-in desktop is available"));
    }
    let mut pid = 0u32;
    // SAFETY: the window handle came from GetShellWindow; `pid` is a valid out pointer.
    unsafe { GetWindowThreadProcessId(shell, &mut pid) };
    if pid == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the id is a live process and the access right only allows reading about it.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let outcome = (|| {
        let mut token: HANDLE = null_mut();
        // SAFETY: `process` is a live handle; the token is closed below.
        if unsafe { OpenProcessToken(process, TOKEN_DUPLICATE | TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut primary: HANDLE = null_mut();
        // SAFETY: the token is live, and `primary` receives a duplicate closed below.
        let duplicated = unsafe {
            DuplicateTokenEx(
                token,
                TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_QUERY,
                null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut primary,
            )
        };
        // SAFETY: the token came from OpenProcessToken and is closed once.
        unsafe { CloseHandle(token) };
        if duplicated == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = start_as_user(primary, dir);
        // SAFETY: the duplicate token is closed once.
        unsafe { CloseHandle(primary) };
        result
    })();
    // SAFETY: the process handle came from OpenProcess and is closed once.
    unsafe { CloseHandle(process) };
    outcome
}

fn start_as_user(token: HANDLE, dir: &Path) -> io::Result<()> {
    let program = dir.join("bs-window.exe");
    let application = wide_path(&program);
    // CreateProcessWithTokenW may write to the command line, so it needs its own buffer.
    let mut command = wide_path(&program);
    let desktop = wide("winsta0\\default");
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: desktop.as_ptr() as *mut u16,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: the token is a live primary token, both strings are NUL-terminated and
    // outlive the call, and the two handles it fills in are closed below.
    let started = unsafe {
        CreateProcessWithTokenW(
            token,
            LOGON_WITH_PROFILE,
            application.as_ptr(),
            command.as_mut_ptr(),
            0,
            null(),
            null(),
            &startup,
            &mut info,
        )
    };
    if started == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both handles were filled in by the successful call and are closed once.
    unsafe {
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
    }
    Ok(())
}

struct Sc(windows_sys::Win32::System::Services::SC_HANDLE);
impl Drop for Sc {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

fn manager(access: u32) -> io::Result<Sc> {
    // SAFETY: null machine and database select the local default SCM.
    let handle = unsafe { OpenSCManagerW(null(), null(), access) };
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Sc(handle))
    }
}

fn open_service(manager: &Sc) -> io::Result<Option<Sc>> {
    let name = wide(SERVICE);
    let handle = unsafe { OpenServiceW(manager.0, name.as_ptr(), SERVICE_ALL_ACCESS) };
    if !handle.is_null() {
        return Ok(Some(Sc(handle)));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
        Ok(None)
    } else {
        Err(err)
    }
}

fn create_service(manager: &Sc, file: &Path) -> io::Result<Sc> {
    let name = wide(SERVICE);
    let display = wide("better_search");
    let binary = wide(&format!("\"{}\"", file.display()));
    // SAFETY: NUL-terminated strings and null optional arguments. Null account runs
    // the service as LocalSystem; its own startup secures ProgramData and the pipe.
    let handle = unsafe {
        CreateServiceW(
            manager.0,
            name.as_ptr(),
            display.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            binary.as_ptr(),
            null(),
            null_mut(),
            null(),
            null(),
            null(),
        )
    };
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Sc(handle))
    }
}

fn stop_service(service: &Sc) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut status = SERVICE_STATUS::default();
    unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) };
    loop {
        let mut current = SERVICE_STATUS_PROCESS::default();
        let mut read = 0;
        // SAFETY: `current` has the layout and size expected by this information level.
        if unsafe {
            QueryServiceStatusEx(
                service.0,
                SC_STATUS_PROCESS_INFO,
                (&raw mut current).cast(),
                size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut read,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if current.dwCurrentState == SERVICE_STOPPED {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("service did not stop in 30 seconds"));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn wait_started(service: &Sc) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut current = SERVICE_STATUS_PROCESS::default();
        let mut read = 0;
        // SAFETY: the buffer layout and size match SC_STATUS_PROCESS_INFO.
        if unsafe {
            QueryServiceStatusEx(
                service.0,
                SC_STATUS_PROCESS_INFO,
                (&raw mut current).cast(),
                size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut read,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if current.dwCurrentState == SERVICE_RUNNING {
            return Ok(());
        }
        if current.dwCurrentState == SERVICE_STOPPED {
            return Err(io::Error::other("service stopped during startup"));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("service did not start in 15 seconds"));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn reg_text(subkey: &str, name: &str, value: &str) -> io::Result<()> {
    let bytes = wide(value);
    let error = unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            wide(subkey).as_ptr(),
            wide(name).as_ptr(),
            REG_SZ,
            bytes.as_ptr().cast(),
            (bytes.len() * 2) as u32,
        )
    };
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error as i32))
    }
}

fn reg_number(subkey: &str, name: &str, value: u32) -> io::Result<()> {
    let error = unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            wide(subkey).as_ptr(),
            wide(name).as_ptr(),
            REG_DWORD,
            (&raw const value).cast(),
            size_of::<u32>() as u32,
        )
    };
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error as i32))
    }
}

fn reg_text_value(subkey: &str, name: &str) -> io::Result<Option<String>> {
    let mut buffer = [0u16; 1024];
    let mut bytes = size_of_val(&buffer) as u32;
    // SAFETY: NUL-terminated names and an output buffer sized in bytes.
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide(subkey).as_ptr(),
            wide(name).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if code == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(Some(String::from_utf16_lossy(&buffer[..end])))
}

fn registered_install(dir: &Path) -> io::Result<bool> {
    Ok(reg_text_value(UNINSTALL_KEY, "InstallLocation")?
        .is_some_and(|location| location.eq_ignore_ascii_case(&dir.display().to_string())))
}

fn remove_registration() -> io::Result<()> {
    let run = unsafe {
        RegDeleteKeyValueW(
            HKEY_LOCAL_MACHINE,
            wide(RUN_KEY).as_ptr(),
            wide(SERVICE).as_ptr(),
        )
    };
    if run != 0 && run != windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND {
        return Err(io::Error::from_raw_os_error(run as i32));
    }
    let app = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, wide(UNINSTALL_KEY).as_ptr()) };
    if app != 0 && app != windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND {
        return Err(io::Error::from_raw_os_error(app as i32));
    }
    Ok(())
}

fn register_app(dir: &Path, self_file: &Path) -> io::Result<()> {
    reg_text(
        RUN_KEY,
        SERVICE,
        &format!("\"{}\" --hidden", dir.join("bs-window.exe").display()),
    )?;
    reg_text(UNINSTALL_KEY, "DisplayName", "better_search")?;
    reg_text(UNINSTALL_KEY, "DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    reg_text(UNINSTALL_KEY, "Publisher", "better_search")?;
    reg_text(UNINSTALL_KEY, "InstallLocation", &dir.display().to_string())?;
    reg_text(
        UNINSTALL_KEY,
        "UninstallString",
        &format!("\"{}\" --uninstall", self_file.display()),
    )?;
    reg_number(UNINSTALL_KEY, "NoModify", 1)?;
    reg_number(UNINSTALL_KEY, "NoRepair", 1)
}

/// Writes `bytes` over `file`. Windows lets a running program's image be renamed but
/// not overwritten, so a locked file is moved aside first; the running copy keeps
/// working from the renamed file until it exits.
fn replace_file(file: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let temp = file.with_file_name(format!("{name}.new"));
    {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        io::Write::write_all(&mut out, bytes)?;
    }
    if std::fs::rename(&temp, file).is_ok() {
        return Ok(());
    }
    let old = file.with_file_name(format!("{name}.old"));
    let _ = std::fs::remove_file(&old);
    if let Err(err) = std::fs::rename(file, &old) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp, file) {
        // Put the original back, so an installed program is never left missing.
        let _ = std::fs::rename(&old, file);
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    let _ = std::fs::remove_file(&old);
    Ok(())
}

/// Deletes the images an upgrade renamed aside, with the same treatment as an uninstall
/// because one of them may still be running: an upgrade renames the panel's image while
/// the panel keeps working from it, and Windows refuses to delete a running image.
/// Returns the first leftover that could not be moved out of the folder.
fn clear_upgrade_leftovers(dir: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = FILES
        .iter()
        .map(|&(name, _)| dir.join(format!("{name}.old")))
        .collect();
    candidates.push(dir.join("better-search-setup.exe.old"));
    let mut stuck = None;
    for path in candidates {
        if !delete_or_move_out(&path) && stuck.is_none() {
            stuck = Some(path);
        }
    }
    stuck
}

/// The files an uninstall or an upgrade can leave behind: the setup program that was
/// still running when the folder was cleared, and the renamed-old image of an upgrade.
fn is_our_leftover(name: &str) -> bool {
    if name == "better-search-setup.exe" {
        return true;
    }
    let Some(base) = name.strip_suffix(".old") else {
        return false;
    };
    base == "better-search-setup.exe" || FILES.iter().any(|&(program, _)| program == base)
}

/// True when the folder holds nothing but our own leftovers, so a fresh install may take
/// it over instead of refusing. Anything else means the folder is not ours to reuse.
fn only_leftovers(dir: &Path) -> io::Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || !is_our_leftover(&entry.file_name().to_string_lossy()) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Removes the leftovers so a fresh install can write its own files. Unlike
/// `clear_upgrade_leftovers` this reports failures: a leftover that is still running
/// would otherwise fail later with a message about a file that already exists.
fn clear_leftovers(dir: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if is_our_leftover(&entry.file_name().to_string_lossy()) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Asks Windows to delete a file at the next reboot, the only way to remove a program's
/// image while that program is running. Needs the caller to be elevated.
fn schedule_delete(path: &Path) -> bool {
    // SAFETY: the path is NUL-terminated and the destination is null, which asks for
    // deletion at the next reboot.
    unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            wide_path(path).as_ptr(),
            null(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_DELAY_UNTIL_REBOOT,
        ) != 0
    }
}

/// Deletes `file`, or gets it out of the install folder when Windows refuses to delete a
/// running image (the panel is usually running during an uninstall). Returns whether the
/// file is now out of the way, either gone, moved to the temp folder, or scheduled for
/// deletion where it stands at the next reboot.
fn delete_or_move_out(file: &Path) -> bool {
    if !file.exists() {
        return true;
    }
    if std::fs::remove_file(file).is_ok() {
        return true;
    }
    let name = file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let moved = std::env::temp_dir().join(format!("{name}-removed-{}", std::process::id()));
    if std::fs::rename(file, &moved).is_err() {
        // The temp folder is on another volume, or the move was refused: leave the file
        // where it is and let Windows delete it there at the next reboot.
        return schedule_delete(file);
    }
    let _ = schedule_delete(&moved);
    true
}

/// Replaces an existing installation in place: stops the service, swaps the files
/// (a running window is moved aside, never killed), refreshes the registry and starts
/// the service again. The running window keeps the old code until it is restarted.
fn upgrade(scm: &Sc, dir: &Path) -> io::Result<()> {
    let service = open_service(scm)?;
    if let Some(service) = &service {
        stop_service(service)?;
    }
    let swapped = (|| {
        for &(name, bytes) in &FILES {
            replace_file(&dir.join(name), bytes)?;
        }
        let self_file = dir.join("better-search-setup.exe");
        replace_file(&self_file, &std::fs::read(std::env::current_exe()?)?)?;
        register_app(dir, &self_file)
    })();
    if let Some(service) = service {
        // SAFETY: no arguments; the SCM starts the service under LocalSystem.
        let started = unsafe { StartServiceW(service.0, 0, null()) };
        let running = if started == 0 {
            Err(io::Error::last_os_error())
        } else {
            wait_started(&service)
        };
        swapped?;
        running?;
    } else {
        swapped?;
    }
    // The renamed images go now: the service is stopped, and the panel's copy is moved
    // out of the folder even though the panel keeps running from it.
    clear_upgrade_leftovers(dir);
    Ok(())
}

fn install() -> io::Result<()> {
    let dir = install_dir()?;
    let scm = manager(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE)?;
    if registered_install(&dir)? {
        return upgrade(&scm, &dir);
    }
    if open_service(&scm)?.is_some()
        || reg_text_value(RUN_KEY, SERVICE)?.is_some()
        || reg_text_value(UNINSTALL_KEY, "DisplayName")?.is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "better_search is already installed; nothing was changed",
        ));
    }
    // An uninstall can leave the folder behind, because the copy that was running could
    // not be deleted. Take it over when it holds nothing but our own leftovers, so a
    // reinstall does not have to wait for a restart.
    if dir.exists() {
        if !only_leftovers(&dir)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} exists and holds other files; nothing was changed",
                    dir.display()
                ),
            ));
        }
        clear_leftovers(&dir).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "the copy of the setup program in {} is still in use ({err}); close it and run the installer again",
                    dir.display()
                ),
            )
        })?;
    } else {
        std::fs::create_dir(&dir)?;
    }
    let mut created = Vec::new();
    let outcome = (|| {
        for &(name, bytes) in &FILES {
            let file = dir.join(name);
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)?;
            created.push(file);
            io::Write::write_all(&mut out, bytes)?;
        }
        let self_file = dir.join("better-search-setup.exe");
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self_file)?;
        created.push(self_file.clone());
        io::copy(
            &mut std::fs::File::open(std::env::current_exe()?)?,
            &mut out,
        )?;
        drop(out);

        let service = create_service(&scm, &dir.join("bs-service.exe"))?;
        let registered = (|| {
            register_app(&dir, &self_file)?;
            // SAFETY: no arguments; SCM starts under LocalSystem.
            if unsafe { StartServiceW(service.0, 0, null()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            wait_started(&service)?;
            Ok(())
        })();
        if registered.is_err() {
            let _ = stop_service(&service);
            unsafe { DeleteService(service.0) };
            let _ = remove_registration();
        }
        registered
    })();
    if let Err(err) = outcome {
        for file in created.into_iter().rev() {
            let _ = std::fs::remove_file(file);
        }
        let _ = std::fs::remove_dir(&dir);
        return Err(err);
    }
    Ok(())
}

/// Asks a running search panel to exit before the files are removed, so its program image
/// is released and can be deleted rather than moved out of the folder. The window class
/// and the message are better_search's own; a panel from an older version ignores the
/// message and keeps running, in which case the file is moved aside instead.
fn close_panel() {
    /// `WM_APP + 9` in the search window: quit, exactly as the tray's Quit does.
    const WM_QUIT_PANEL: u32 = 0x8000 + 9;
    let class = wide("BetterSearchWindow");
    // SAFETY: NUL-terminated class name, no window title filter.
    let hwnd = unsafe { FindWindowW(class.as_ptr(), null()) };
    if hwnd.is_null() {
        return;
    }
    // SAFETY: the handle came from FindWindowW, and the message takes no arguments.
    unsafe { PostMessageW(hwnd, WM_QUIT_PANEL, 0, 0) };
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
        // SAFETY: as above; the class is still registered while a window of it exists.
        if unsafe { FindWindowW(class.as_ptr(), null()) }.is_null() {
            return;
        }
    }
}

fn uninstall() -> io::Result<()> {
    let dir = install_dir()?;
    let scm = manager(SC_MANAGER_CONNECT)?;
    let registered = registered_install(&dir)?;
    let service = open_service(&scm)?;
    if !registered {
        // Without our own registration nothing here is ours to delete, except that a
        // missing service alone is not a reason to refuse: an earlier run may have
        // removed it and then stopped.
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            if service.is_some() {
                "the installer registration is missing; refusing to remove an unknown service"
            } else {
                "better_search is not installed (no service and no installer registration)"
            },
        ));
    }
    if let Some(service) = &service {
        stop_service(service)?;
        // SAFETY: the service is stopped and the handle stays valid for the delete call.
        if unsafe { DeleteService(service.0) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    remove_registration()?;
    // The panel is usually running, and Windows will not delete a running image. Ask it to
    // exit first; if it does not (an older version, or another user's session), the copy
    // is moved out of the folder and deleted at the next reboot instead of blocking the
    // uninstall or lingering in Program Files.
    close_panel();
    let mut stuck = None;
    for path in FILES
        .iter()
        .map(|&(name, _)| dir.join(name))
        .chain([dir.join("better-search-setup.exe")])
    {
        if !delete_or_move_out(&path) && stuck.is_none() {
            stuck = Some(path);
        }
    }
    if let Some(leftover) = clear_upgrade_leftovers(&dir)
        && stuck.is_none()
    {
        stuck = Some(leftover);
    }
    let question = wide("Delete the saved index and service logs? Choose No to keep them.");
    let title = wide("better_search uninstall");
    let erase = unsafe {
        MessageBoxW(
            null_mut(),
            question.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
        )
    } == windows_sys::Win32::UI::WindowsAndMessaging::IDYES;
    if erase {
        let data = data_dir()?;
        for name in ["index.bin", "service.log", "service.log.1"] {
            let path = data.join(name);
            if path.is_file() {
                std::fs::remove_file(path)?;
            }
        }
        let _ = std::fs::remove_dir(data); // Never recursively delete unknown content.
    }
    // The uninstaller is running from this folder too, so it is moved out the same way.
    let self_file = dir.join("better-search-setup.exe");
    if !delete_or_move_out(&self_file) && stuck.is_none() {
        stuck = Some(self_file);
    }
    // Unknown files keep the folder, which is not ours to delete; ours are out of it.
    let _ = std::fs::remove_dir(&dir);
    match stuck {
        Some(file) => Err(io::Error::other(format!(
            "could not remove {}; close it and run the uninstall again",
            file.display()
        ))),
        None => Ok(()),
    }
}

/// Prints to the console this program was started from, if there is one. A program built
/// without a console subsystem has no working standard output even after attaching, so the
/// console is opened by name. Returns false when there is no console to print to.
fn print_console(text: &str) -> bool {
    // SAFETY: asks for the console of the program that started this one.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return false;
    }
    let name = wide("CONOUT$");
    // SAFETY: the name is NUL-terminated; the handle is closed below.
    let console = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if console.is_null() || console == INVALID_HANDLE_VALUE {
        return false;
    }
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let mut written = 0u32;
    // SAFETY: the buffer holds `utf16.len()` code units and the handle is live.
    unsafe {
        WriteConsoleW(
            console,
            utf16.as_ptr(),
            utf16.len() as u32,
            &mut written,
            null(),
        )
    };
    // SAFETY: the handle came from CreateFileW and is closed once.
    unsafe { CloseHandle(console) };
    true
}

/// `--inspect` says what the setup program would install and changes nothing. It borrows
/// the console it was started from; with none (a double-click) the same text is shown in a
/// message box, because this program has no console of its own.
fn inspect() {
    let mut text = format!("better_search setup {}\n", env!("CARGO_PKG_VERSION"));
    match install_dir() {
        Ok(dir) => text.push_str(&format!("Install folder: {}\n", dir.display())),
        Err(err) => text.push_str(&format!("Install folder: unavailable ({err})\n")),
    }
    for &(name, bytes) in &FILES {
        text.push_str(&format!("{name}: {} bytes\n", bytes.len()));
    }
    text.push_str("Inspect does not install, elevate or modify the machine.\n");
    if !print_console(&text) {
        note("better_search setup", &text, MB_ICONINFORMATION);
    }
}

/// The ordinary double-click path. The machine changes need an administrator, so they run
/// in an elevated copy of this program; this copy stays unelevated, waits for that to
/// finish, and then starts the search panel as the user. The panel is registered to start
/// at sign-in, which on its own would leave a fresh install with nothing to press Alt+Space
/// in until then. A setup program that is already elevated does the install itself and
/// falls back to borrowing the shell's token for the panel.
fn install_entry() -> io::Result<()> {
    if elevated().unwrap_or(false) {
        install()?;
    } else {
        request_elevation("--install-elevated")?;
    }
    start_panel(&install_dir()?);
    Ok(())
}

fn main() {
    let command = std::env::args().nth(1);
    if command.as_deref() == Some("--inspect") {
        inspect();
        return;
    }
    let result = match command.as_deref() {
        None => install_entry(),
        Some("--install-elevated") => {
            if elevated().unwrap_or(false) {
                install()
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "elevation required",
                ))
            }
        }
        Some("--uninstall") => {
            if elevated().unwrap_or(false) {
                uninstall()
            } else {
                request_elevation("--uninstall-elevated")
            }
        }
        Some("--uninstall-elevated") => {
            if elevated().unwrap_or(false) {
                uninstall()
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "elevation required",
                ))
            }
        }
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown option",
        )),
    };
    if let Err(err) = result {
        note(
            "better_search setup",
            &format!("better_search setup could not finish.\n\n{err}"),
            MB_ICONERROR,
        );
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_programs_are_windows_executables() {
        assert!(
            FILES
                .iter()
                .all(|(_, bytes)| bytes.len() > 1024 && bytes.starts_with(b"MZ"))
        );
        assert!(install_dir().unwrap().ends_with("better_search"));
        assert!(data_dir().unwrap().ends_with("better_search"));
    }

    /// A scratch folder for the file-swap tests; nothing here is installed.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bs-swap-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn replacing_a_file_writes_through_and_leaves_nothing_behind() {
        let dir = scratch("free");
        let file = dir.join("bs-window.exe");
        replace_file(&file, b"first").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"first");
        replace_file(&file, b"second").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"second");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no scratch files"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An upgrade renames a running program's image aside, and the uninstall clears
    /// those copies. The current programs are left alone, and a copy that cannot be
    /// removed is reported rather than silently keeping the folder.
    #[test]
    fn leftovers_from_an_earlier_upgrade_are_removed() {
        let dir = scratch("leftovers");
        std::fs::write(dir.join("bs-window.exe"), b"current").unwrap();
        std::fs::write(dir.join("bs-window.exe.old"), b"old").unwrap();
        std::fs::write(dir.join("bs-service.exe.old"), b"old").unwrap();
        std::fs::write(dir.join("better-search-setup.exe.old"), b"old").unwrap();
        assert_eq!(clear_upgrade_leftovers(&dir), None);
        assert_eq!(
            std::fs::read(dir.join("bs-window.exe")).unwrap(),
            b"current"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A running program's image denies write access but allows delete sharing, which
    /// is what lets `replace_file` swap the file under a running better_search. A file
    /// held without delete sharing fails cleanly and is left untouched.
    #[test]
    fn replacing_a_file_without_delete_sharing_fails_without_damage() {
        let dir = scratch("locked");
        let file = dir.join("bs-window.exe");
        std::fs::write(&file, b"old").unwrap();
        let name = wide_path(&file);
        // SAFETY: NUL-terminated path, default security attributes, existing file.
        let handle = unsafe {
            windows_sys::Win32::Storage::FileSystem::CreateFileW(
                name.as_ptr(),
                windows_sys::Win32::Foundation::GENERIC_READ,
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
                null(),
                windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        assert_ne!(
            handle,
            windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE,
            "could not hold the file open: {}",
            io::Error::last_os_error()
        );
        let outcome = replace_file(&file, b"new");
        // SAFETY: the handle came from CreateFileW above and is closed once.
        unsafe { CloseHandle(handle) };
        assert!(
            outcome.is_err(),
            "a file without delete sharing must not be replaced"
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"old");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no scratch files"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that can be neither deleted nor moved is reported, so the uninstall tells
    /// the user to close it instead of leaving the folder behind without a word. (A
    /// running image is the real case: Windows refuses to delete it but allows a rename,
    /// which the end-to-end run on the development machine covers.)
    #[test]
    fn a_file_that_cannot_be_removed_is_reported() {
        let dir = scratch("stuck");
        let file = dir.join("bs-window.exe.old");
        std::fs::write(&file, b"running").unwrap();
        let name = wide_path(&file);
        // SAFETY: NUL-terminated path, default security attributes, existing file.
        let handle = unsafe {
            windows_sys::Win32::Storage::FileSystem::CreateFileW(
                name.as_ptr(),
                windows_sys::Win32::Foundation::GENERIC_READ,
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
                null(),
                windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        assert_ne!(handle, windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE);
        let stuck = clear_upgrade_leftovers(&dir);
        // SAFETY: the handle came from CreateFileW above and is closed once.
        unsafe { CloseHandle(handle) };
        assert_eq!(stuck, Some(file.clone()));
        assert!(file.exists());
        assert_eq!(clear_upgrade_leftovers(&dir), None, "now it can go");
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_our_own_files_count_as_leftovers() {
        assert!(is_our_leftover("better-search-setup.exe"));
        assert!(is_our_leftover("better-search-setup.exe.old"));
        assert!(is_our_leftover("bs-window.exe.old"));
        assert!(is_our_leftover("bs-service.exe.old"));
        assert!(!is_our_leftover("bs-window.exe"));
        assert!(!is_our_leftover("index.bin"));
        assert!(!is_our_leftover("notes.txt"));
        assert!(!is_our_leftover("bs-window.exe.old.bak"));
    }

    /// The folder an uninstall can leave behind is taken over by a fresh install, but
    /// only when it holds nothing but our own files.
    #[test]
    fn a_folder_of_leftovers_is_adopted_and_any_other_file_stops_that() {
        let dir = scratch("adopt");
        std::fs::write(dir.join("better-search-setup.exe"), b"old").unwrap();
        std::fs::write(dir.join("bs-window.exe.old"), b"old").unwrap();
        assert!(only_leftovers(&dir).unwrap());
        clear_leftovers(&dir).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        std::fs::write(dir.join("better-search-setup.exe"), b"old").unwrap();
        std::fs::write(dir.join("someone-elses-file.txt"), b"keep").unwrap();
        assert!(!only_leftovers(&dir).unwrap());
        clear_leftovers(&dir).unwrap();
        assert!(!dir.join("better-search-setup.exe").exists());
        assert!(dir.join("someone-elses-file.txt").exists());

        std::fs::create_dir(dir.join("sub")).unwrap();
        assert!(!only_leftovers(&dir).unwrap(), "a folder is not a leftover");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_free_file_is_removed_and_a_missing_one_needs_no_work() {
        let dir = scratch("remove");
        let file = dir.join("bs.exe");
        assert!(delete_or_move_out(&file), "a missing file is already gone");
        std::fs::write(&file, b"old").unwrap();
        assert!(delete_or_move_out(&file));
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
