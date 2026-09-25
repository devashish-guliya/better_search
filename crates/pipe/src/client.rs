use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_MORE_DATA, ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE,
};
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
    pub fn connect() -> io::Result<Self> {
        let name: Vec<u16> = PIPE_NAME.encode_utf16().chain([0]).collect();
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
        Reply::decode(&self.reply[..len])
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed reply"))
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
