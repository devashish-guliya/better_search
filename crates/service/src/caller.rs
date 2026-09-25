//! Finds out which user is on the other end of a pipe connection.

use std::ffi::c_void;
use std::io;
use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    GetTokenInformation, RevertToSelf, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

pub struct Caller {
    pub sid: String,
    /// The user's profile folder, e.g. `C:\Users\anna`, if the user has one.
    pub profile: Option<PathBuf>,
}

/// Identifies the client of `pipe`. Needs at least one message read from it first.
pub fn identify(pipe: HANDLE) -> io::Result<Caller> {
    let token = client_token(pipe)?;
    let sid = token_user_sid(token);
    // SAFETY: the token was opened by `client_token`.
    unsafe { CloseHandle(token) };
    let sid = sid?;
    let profile = profile_path(&sid);
    Ok(Caller { sid, profile })
}

fn client_token(pipe: HANDLE) -> io::Result<HANDLE> {
    // SAFETY: `pipe` is a connected server end.
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut token: HANDLE = null_mut();
    // "As self": the token object is opened with the service's own rights, since the
    // client may only have allowed identification.
    // SAFETY: the pseudo handle needs no closing; `token` receives a new handle.
    let ok = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    let err = io::Error::last_os_error();
    // SAFETY: ends the impersonation started above.
    if unsafe { RevertToSelf() } == 0 {
        // Carrying on as the client would be a security bug.
        std::process::abort();
    }
    if ok == 0 {
        return Err(err);
    }
    Ok(token)
}

fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let mut needed = 0u32;
    // SAFETY: a size query with no buffer.
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed) };
    // u64 elements keep the buffer aligned for TOKEN_USER.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: the buffer holds at least `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation filled the buffer with a TOKEN_USER.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = null_mut();
    // SAFETY: the SID points into `buffer`; `text` receives a LocalAlloc'd string.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `text` is a NUL-terminated string allocated by the call above.
    let sid = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        sid
    };
    Ok(sid)
}

/// Looks up the profile folder Windows records for `sid`.
fn profile_path(sid: &str) -> Option<PathBuf> {
    let key: Vec<u16> = format!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\{sid}")
        .encode_utf16()
        .chain([0])
        .collect();
    let value: Vec<u16> = "ProfileImagePath".encode_utf16().chain([0]).collect();
    let mut buffer = [0u16; 520];
    let mut size = (buffer.len() * 2) as u32;
    // RRF_RT_REG_SZ also accepts REG_EXPAND_SZ values and expands them.
    // SAFETY: key and value names are NUL-terminated; `size` is the buffer size in bytes.
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &mut size,
        )
    };
    if err != ERROR_SUCCESS {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_system_profile() {
        let path = profile_path("S-1-5-18").expect("LocalSystem has a profile entry");
        assert!(
            path.to_string_lossy()
                .to_lowercase()
                .ends_with(r"\system32\config\systemprofile"),
            "{}",
            path.display()
        );
        assert_eq!(profile_path("S-1-5-21-0-0-0-99999"), None);
    }
}
