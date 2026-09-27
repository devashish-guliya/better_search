//! Security descriptors for the pipe and the data folder.

use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SetKernelObjectSecurity,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, MoveFileExW, OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
};

/// Pipe access: network logons are refused; SYSTEM and elevated administrators have full
/// access; interactive users may connect, read, write and switch to message mode
/// (`0x12018b` = generic read + write data + write attributes). Ordinary users do not
/// get `FILE_CREATE_PIPE_INSTANCE`, so they cannot serve fake instances of the pipe.
/// Owned by Administrators, which an ordinary user cannot make an object's owner: the
/// client checks the owner, so a pipe of this name created by another program while
/// the service is not running is refused (see `bs_pipe::Client::connect`).
pub const PIPE_SDDL: &str = "O:BAD:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12018b;;;IU)";

/// Data folder: owned by Administrators, only SYSTEM and administrators, nothing
/// inherited from ProgramData (which lets every user read). The snapshot lists every
/// user's file names.
const DATA_SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

/// SYSTEM and Administrators: the only owners a trusted data folder can have.
const TRUSTED_OWNERS: &[&str] = &["S-1-5-18", "S-1-5-32-544"];

/// Attempts to end up with a trusted folder before giving up; each failed attempt
/// means something replaced the folder in between.
const ATTEMPTS: usize = 4;

pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is immutable after creation and only freed on drop.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let wide: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `wide` is NUL-terminated; the size output is optional.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(sd))
    }

    pub fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// `S-1-5-18` style text for a SID.
pub fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = null_mut();
    // SAFETY: `sid` is valid for the call; `text` receives a LocalAlloc'd string.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `text` is a NUL-terminated string allocated by the call above.
    unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        Ok(sid)
    }
}

/// Makes `dir` a folder only SYSTEM and administrators can use, and returns lines for
/// the log about anything it had to move out of the way.
///
/// Every user may create folders in ProgramData, so a folder found there may have been
/// planted before the service first ran: a junction (following it would change the
/// permissions of, and write files into, whatever it points at), a folder owned by a
/// user (an owner can always rewrite the permissions and read the index), or a trusted
/// folder with planted files inside. Such a folder is renamed aside, never used, and a
/// fresh one is created.
pub fn secure_dir(dir: &Path) -> io::Result<Vec<String>> {
    secure_dir_with(dir, DATA_SDDL, TRUSTED_OWNERS)
}

fn secure_dir_with(dir: &Path, sddl: &str, trusted: &[&str]) -> io::Result<Vec<String>> {
    let sd = SecurityDescriptor::from_sddl(sddl)?;
    let mut notes = Vec::new();
    for _ in 0..ATTEMPTS {
        let access = READ_CONTROL | WRITE_DAC | WRITE_OWNER | FILE_READ_ATTRIBUTES;
        let reason = match Handle::open_no_follow(dir, access) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                create_dir(dir, &sd)?;
                continue;
            }
            Err(e) => format!("it could not be opened ({e})"),
            Ok(handle) => match untrusted_dir(&handle, trusted) {
                Some(reason) => reason,
                None => {
                    // Set through the handle, so it is certainly the folder just checked.
                    // No propagation to what is inside: that is checked right after, and
                    // files the service creates inherit these entries anyway.
                    handle.set_security(&sd)?;
                    drop(handle);
                    match untrusted_child(dir, trusted)? {
                        None => return Ok(notes),
                        Some(reason) => reason,
                    }
                }
            },
        };
        let aside = move_aside(dir)?;
        notes.push(format!(
            "moved {} aside to {} and did not use it: {reason}",
            dir.display(),
            aside.display()
        ));
    }
    Err(io::Error::other(format!(
        "{} kept being replaced by something untrusted",
        dir.display()
    )))
}

/// Why the folder behind `handle` must not be used, if it must not.
fn untrusted_dir(handle: &Handle, trusted: &[&str]) -> Option<String> {
    let info = match handle.info() {
        Ok(info) => info,
        Err(e) => return Some(format!("its attributes could not be read ({e})")),
    };
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Some("it is a junction or link".into());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Some("it is not a folder".into());
    }
    untrusted_owner(handle, trusted)
}

fn untrusted_owner(handle: &Handle, trusted: &[&str]) -> Option<String> {
    match handle.owner() {
        Ok(owner) if trusted.contains(&owner.as_str()) => None,
        Ok(owner) => Some(format!("it is owned by {owner}")),
        Err(e) => Some(format!("its owner could not be read ({e})")),
    }
}

/// Why something inside `dir` makes the folder untrustworthy, if it does. Only called
/// once `dir` is locked down, so nothing can be added meanwhile.
fn untrusted_child(dir: &Path, trusted: &[&str]) -> io::Result<Option<String>> {
    for item in std::fs::read_dir(dir)? {
        let path = item?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let handle = match Handle::open_no_follow(&path, READ_CONTROL | FILE_READ_ATTRIBUTES) {
            Ok(handle) => handle,
            Err(e) => return Ok(Some(format!("{name} inside could not be opened ({e})"))),
        };
        let info = match handle.info() {
            Ok(info) => info,
            Err(e) => return Ok(Some(format!("{name} inside could not be read ({e})"))),
        };
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(Some(format!("{name} inside is a junction or link")));
        }
        // A hard link would make the service write into a file that also lives
        // somewhere else.
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 && info.nNumberOfLinks != 1 {
            return Ok(Some(format!("{name} inside has more than one name")));
        }
        if let Some(reason) = untrusted_owner(&handle, trusted) {
            return Ok(Some(format!("{name} inside: {reason}")));
        }
    }
    Ok(None)
}

fn create_dir(dir: &Path, sd: &SecurityDescriptor) -> io::Result<()> {
    let wide = wide_nul(dir);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.as_ptr(),
        bInheritHandle: 0,
    };
    // SAFETY: `wide` is NUL-terminated and `attributes` points to a valid descriptor.
    if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } == 0 {
        let err = io::Error::last_os_error();
        // Someone else created it just now; the next round checks what it is.
        if err.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(err);
        }
    }
    Ok(())
}

/// Renames `dir` (the link itself, for a junction) to an unused name next to it.
fn move_aside(dir: &Path) -> io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    let aside = dir.with_file_name(format!("{name}.untrusted-{stamp}-{}", std::process::id()));
    let (from, to) = (wide_nul(dir), wide_nul(&aside));
    // SAFETY: both names are NUL-terminated.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(aside)
}

/// A file or folder handle that never went through a junction or link at the end of
/// the path.
struct Handle(HANDLE);

impl Handle {
    fn open_no_follow(path: &Path, access: u32) -> io::Result<Self> {
        let wide = wide_nul(path);
        // SAFETY: `wide` is NUL-terminated; no security attributes or template.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                // Backup semantics is needed to open folders at all.
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    fn info(&self) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
        // SAFETY: zero is a valid value for this plain struct, which the call fills.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open.
        if unsafe { GetFileInformationByHandle(self.0, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info)
    }

    fn owner(&self) -> io::Result<String> {
        let mut owner: PSID = null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: the handle has READ_CONTROL; outputs point to locals.
        let err = unsafe {
            GetSecurityInfo(
                self.0,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                null_mut(),
                null_mut(),
                &mut sd,
            )
        };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        let text = sid_string(owner);
        // SAFETY: `owner` points into `sd`, which GetSecurityInfo allocated.
        unsafe { LocalFree(sd.cast::<c_void>()) };
        text
    }

    /// Replaces owner and permissions of this object only.
    fn set_security(&self, sd: &SecurityDescriptor) -> io::Result<()> {
        let what = OWNER_SECURITY_INFORMATION
            | DACL_SECURITY_INFORMATION
            | PROTECTED_DACL_SECURITY_INFORMATION;
        // SAFETY: the handle has WRITE_DAC and WRITE_OWNER; the descriptor is valid.
        if unsafe { SetKernelObjectSecurity(self.0, what, sd.as_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: opened in `open_no_follow` and closed only here.
        unsafe { CloseHandle(self.0) };
    }
}

fn wide_nul(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorControl, SE_DACL_PROTECTED, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    #[test]
    fn descriptors_parse() {
        SecurityDescriptor::from_sddl(PIPE_SDDL).unwrap();
        SecurityDescriptor::from_sddl(DATA_SDDL).unwrap();
    }

    /// The current user stands in for SYSTEM and Administrators, so the checks can run
    /// without elevation.
    struct Sandbox {
        root: PathBuf,
        me: String,
        sddl: String,
    }

    impl Sandbox {
        fn new(test: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("bs_secure_dir_{test}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let me = current_user();
            let sddl = format!("O:{me}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{me})");
            Self { root, me, sddl }
        }

        fn dir(&self) -> PathBuf {
            self.root.join("data")
        }

        fn secure(&self) -> Vec<String> {
            secure_dir_with(&self.dir(), &self.sddl, &[self.me.as_str()]).unwrap()
        }

        fn moved_aside(&self) -> Vec<PathBuf> {
            std::fs::read_dir(&self.root)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.to_string_lossy().contains("data.untrusted-"))
                .collect()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            // Junctions first, as links, so nothing behind them is touched.
            for path in self.moved_aside() {
                let _ = std::fs::remove_dir(&path);
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn current_user() -> String {
        let mut token: HANDLE = null_mut();
        // SAFETY: the pseudo handle needs no closing; `token` receives a new handle.
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
            0
        );
        let sid = crate::caller::token_user_sid(token).unwrap();
        // SAFETY: opened above.
        unsafe { CloseHandle(token) };
        sid
    }

    fn is_protected(path: &Path) -> bool {
        let handle = Handle::open_no_follow(path, READ_CONTROL).unwrap();
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: the handle has READ_CONTROL; outputs point to locals.
        let err = unsafe {
            GetSecurityInfo(
                handle.0,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut sd,
            )
        };
        assert_eq!(err, 0);
        let (mut control, mut revision) = (0u16, 0u32);
        // SAFETY: `sd` was allocated by GetSecurityInfo.
        unsafe {
            assert_ne!(
                GetSecurityDescriptorControl(sd, &mut control, &mut revision),
                0
            );
            LocalFree(sd.cast::<c_void>());
        }
        control & SE_DACL_PROTECTED != 0
    }

    #[test]
    fn creates_a_missing_folder_and_keeps_a_trusted_one() {
        let sandbox = Sandbox::new("create");
        assert!(sandbox.secure().is_empty());
        assert!(sandbox.dir().is_dir());
        assert!(is_protected(&sandbox.dir()));

        std::fs::write(sandbox.dir().join("index.bin"), b"kept").unwrap();
        assert!(sandbox.secure().is_empty());
        assert_eq!(
            std::fs::read(sandbox.dir().join("index.bin")).unwrap(),
            b"kept"
        );
        assert!(sandbox.moved_aside().is_empty());
    }

    #[test]
    fn moves_a_junction_aside_without_touching_its_target() {
        let sandbox = Sandbox::new("junction");
        let target = sandbox.root.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("precious.txt"), b"untouched").unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(sandbox.dir())
            .arg(&target)
            .output()
            .unwrap()
            .status;
        assert!(status.success());

        let notes = sandbox.secure();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("junction"), "{notes:?}");
        // A real, empty folder now, and the junction was renamed rather than followed.
        let info = Handle::open_no_follow(&sandbox.dir(), FILE_READ_ATTRIBUTES)
            .unwrap()
            .info()
            .unwrap();
        assert_eq!(info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT, 0);
        assert_eq!(std::fs::read_dir(sandbox.dir()).unwrap().count(), 0);
        assert_eq!(sandbox.moved_aside().len(), 1);
        assert!(!is_protected(&target));
        assert_eq!(
            std::fs::read(target.join("precious.txt")).unwrap(),
            b"untouched"
        );
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
    }

    #[test]
    fn moves_a_folder_with_a_hard_link_inside_aside() {
        let sandbox = Sandbox::new("hardlink");
        let outside = sandbox.root.join("outside.txt");
        std::fs::write(&outside, b"elsewhere").unwrap();
        std::fs::create_dir(sandbox.dir()).unwrap();
        std::fs::hard_link(&outside, sandbox.dir().join("index.bin")).unwrap();

        let notes = sandbox.secure();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("more than one name"), "{notes:?}");
        assert_eq!(std::fs::read_dir(sandbox.dir()).unwrap().count(), 0);
        assert_eq!(std::fs::read(&outside).unwrap(), b"elsewhere");
    }

    #[test]
    fn a_folder_owned_by_someone_else_is_not_trusted() {
        let sandbox = Sandbox::new("owner");
        std::fs::create_dir(sandbox.dir()).unwrap();
        let handle =
            Handle::open_no_follow(&sandbox.dir(), READ_CONTROL | FILE_READ_ATTRIBUTES).unwrap();
        assert_eq!(untrusted_dir(&handle, &[sandbox.me.as_str()]), None);
        let reason = untrusted_dir(&handle, TRUSTED_OWNERS).expect("not trusted");
        assert!(reason.contains(&sandbox.me), "{reason}");
    }
}
