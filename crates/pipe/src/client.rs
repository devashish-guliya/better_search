use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_MORE_DATA, ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_GENERIC_READ, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, OPEN_EXISTING,
    ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{
    PIPE_READMODE_MESSAGE, SetNamedPipeHandleState, TransactNamedPipe, WaitNamedPipeW,
};

use crate::{PIPE_NAME, Reply, Request};

const READ_CHUNK: usize = 64 * 1024;
const BUSY_WAIT_MS: u32 = 2000;
/// SYSTEM and Administrators. The service makes Administrators the owner of its pipe;
/// an ordinary user can only create objects owned by themselves, so a pipe of the same
/// name made by another program while the service is not running fails this check.
const SERVICE_OWNERS: &[&str] = &["S-1-5-18", "S-1-5-32-544"];

/// A connection to the service. Keep it open while the user types: the service then
/// only re-checks the names that matched the previous keystroke.
pub struct Client {
    handle: HANDLE,
    request: Vec<u8>,
    reply: Vec<u8>,
}

// SAFETY: a pipe handle can be used from any thread.
unsafe impl Send for Client {}

impl Client {
    /// Connects to the service. Refuses (with `PermissionDenied`) a pipe that is not
    /// owned by SYSTEM or Administrators, before any query is sent.
    pub fn connect() -> io::Result<Self> {
        Self::connect_to(PIPE_NAME)
    }

    fn connect_to(pipe_name: &str) -> io::Result<Self> {
        let name: Vec<u16> = pipe_name.encode_utf16().chain([0]).collect();
        loop {
            // Only the rights the pipe grants to ordinary users. Identification lets the
            // service see who is asking without being able to act as that user.
            // SAFETY: `name` is NUL-terminated; other pointers may be null.
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    FILE_GENERIC_READ | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES,
                    0,
                    null(),
                    OPEN_EXISTING,
                    SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                let client = Self {
                    handle,
                    request: Vec::new(),
                    reply: Vec::new(),
                };
                let owner = pipe_owner(client.handle)?;
                if !SERVICE_OWNERS.contains(&owner.as_str()) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "{pipe_name} is served by a program that is not the better_search \
                             service (the pipe is owned by {owner})"
                        ),
                    ));
                }
                let mode = PIPE_READMODE_MESSAGE;
                // SAFETY: the handle is open; unchanged settings are passed as null.
                let ok = unsafe { SetNamedPipeHandleState(client.handle, &mode, null(), null()) };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                return Ok(client);
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                return Err(err);
            }
            // SAFETY: `name` is NUL-terminated.
            if unsafe { WaitNamedPipeW(name.as_ptr(), BUSY_WAIT_MS) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }

    pub fn search(&mut self, query: &str, limit: u16) -> io::Result<Reply> {
        Request {
            query: query.to_owned(),
            limit,
        }
        .encode(&mut self.request);
        let len = self.transact()?;
        Reply::decode(&self.reply[..len])
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed reply"))
    }

    fn transact(&mut self) -> io::Result<usize> {
        self.reply.resize(READ_CHUNK, 0);
        let mut read = 0u32;
        // SAFETY: both buffers are valid for the sizes passed; no overlapped I/O.
        let ok = unsafe {
            TransactNamedPipe(
                self.handle,
                self.request.as_ptr().cast::<c_void>(),
                self.request.len() as u32,
                self.reply.as_mut_ptr().cast::<c_void>(),
                self.reply.len() as u32,
                &mut read,
                null_mut(),
            )
        };
        let mut len = read as usize;
        if ok == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                return Err(err);
            }
            self.read_rest(&mut len)?;
        }
        Ok(len)
    }

    /// Reads the remainder of a reply longer than the first buffer.
    fn read_rest(&mut self, len: &mut usize) -> io::Result<()> {
        loop {
            self.reply.resize(*len + READ_CHUNK, 0);
            let mut read = 0u32;
            // SAFETY: the buffer is valid from `len` for `READ_CHUNK` bytes.
            let ok = unsafe {
                ReadFile(
                    self.handle,
                    self.reply[*len..].as_mut_ptr(),
                    READ_CHUNK as u32,
                    &mut read,
                    null_mut(),
                )
            };
            *len += read as usize;
            if ok != 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                return Err(err);
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: the handle was opened in `connect` and is closed only here.
        unsafe { CloseHandle(self.handle) };
    }
}

/// The owner of the pipe object behind `handle`, as SID text.
fn pipe_owner(handle: HANDLE) -> io::Result<String> {
    let mut owner: PSID = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: the handle was opened with READ_CONTROL; outputs point to locals.
    let err = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
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
    let mut text = null_mut();
    // SAFETY: `owner` points into `sd`; `text` receives a LocalAlloc'd string.
    let converted = unsafe { ConvertSidToStringSidW(owner, &mut text) };
    let result = if converted == 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: `text` is a NUL-terminated string allocated by the call above.
        unsafe {
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text.cast());
            Ok(sid)
        }
    };
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(sd.cast::<c_void>()) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_MESSAGE, PIPE_WAIT};

    #[test]
    fn refuses_a_pipe_that_the_service_did_not_create() {
        let name = format!(r"\\.\pipe\better_search_test_{}", std::process::id());
        let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
        // Created by this (ordinary) test process, as a program squatting on the name
        // while the service is stopped would.
        // SAFETY: `wide` is NUL-terminated; default security.
        let server = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                1,
                4096,
                4096,
                0,
                null(),
            )
        };
        assert_ne!(server, INVALID_HANDLE_VALUE);
        let err = Client::connect_to(&name)
            .err()
            .expect("the pipe must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        assert!(
            err.to_string().contains("not the better_search service"),
            "{err}"
        );
        // SAFETY: created above.
        unsafe { CloseHandle(server) };
    }
}
