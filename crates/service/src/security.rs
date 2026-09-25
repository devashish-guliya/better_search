//! Security descriptors for the pipe and the data folder.

use std::io;
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};

/// Pipe access: network logons are refused; SYSTEM and elevated administrators have full
/// access; interactive users may connect, read, write and switch to message mode
/// (`0x12018b` = generic read + write data + write attributes). Ordinary users do not
/// get `FILE_CREATE_PIPE_INSTANCE`, so they cannot serve fake instances of the pipe.
pub const PIPE_SDDL: &str = "D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12018b;;;IU)";

/// Data folder: only SYSTEM and administrators, nothing inherited from ProgramData
/// (which lets every user read). The snapshot lists every user's file names.
const DATA_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

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

    fn dacl(&self) -> io::Result<*mut ACL> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = null_mut();
        // SAFETY: the descriptor is valid; outputs point to locals.
        let ok =
            unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(dacl)
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// Creates `dir` if needed and restricts it (and everything in it) to SYSTEM and
/// administrators.
pub fn secure_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let sd = SecurityDescriptor::from_sddl(DATA_SDDL)?;
    let wide: Vec<u16> = dir.as_os_str().encode_wide_nul();
    // SAFETY: `wide` is NUL-terminated and the DACL lives as long as `sd`.
    let err = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            sd.dacl()?,
            null_mut(),
        )
    };
    if err != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    Ok(())
}

pub trait EncodeWideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl EncodeWideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain([0]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_parse() {
        SecurityDescriptor::from_sddl(PIPE_SDDL).unwrap();
        let data = SecurityDescriptor::from_sddl(DATA_SDDL).unwrap();
        assert!(!data.dacl().unwrap().is_null());
    }
}
