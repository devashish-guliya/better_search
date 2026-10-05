//! Update check, download and install. Releases live on GitHub; each one carries a
//! small `latest.txt` manifest next to the installer, at a URL that always points at
//! the newest release. This is the only network access better_search makes, it runs
//! only when the user asks for it, and nothing about this machine is sent.

use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_HASH_LENGTH, BCRYPT_OBJECT_LENGTH, BCRYPT_SHA256_ALGORITHM,
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptGetProperty, BCryptHashData, BCryptOpenAlgorithmProvider,
};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::wide;

/// The manifest published with every release. `releases/latest/download` resolves to
/// the newest release, so this URL does not change from release to release.
pub const MANIFEST_URL: &str =
    "https://github.com/devashish-guliya/better_search/releases/latest/download/latest.txt";

/// Test hook: point the check at another address, e.g. a local server. Only the
/// updater reads it, and only when it is set.
const MANIFEST_URL_ENV: &str = "BETTER_SEARCH_UPDATE_URL";

/// A manifest or an installer much larger than this is not what we published.
const MANIFEST_LIMIT: usize = 64 * 1024;
const INSTALLER_LIMIT: usize = 64 * 1024 * 1024;

const AGENT: &str = "better_search-update";

/// What one release advertises: its version, the installer to fetch and the digest
/// that installer must have.
#[derive(Debug, PartialEq)]
pub struct Manifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
}

impl Manifest {
    /// `key=value` lines, as published: `version`, `url`, `sha256`.
    fn parse(text: &str) -> Result<Self, String> {
        let mut fields = std::collections::HashMap::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((name, value)) = line.split_once('=') {
                fields.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        let take = |key: &str| fields.get(key).cloned().unwrap_or_default();
        let manifest = Manifest {
            version: take("version"),
            url: take("url"),
            sha256: take("sha256"),
        };
        if manifest.version.is_empty() || manifest.url.is_empty() || manifest.sha256.is_empty() {
            return Err("the release manifest is incomplete".into());
        }
        if !manifest.url.starts_with("https://") {
            return Err("the release manifest does not use a secure download".into());
        }
        if manifest.sha256.len() != 64 || !manifest.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("the release manifest has a bad checksum".into());
        }
        Ok(manifest)
    }
}

/// The version this program was built as.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// True when `candidate` is a later dotted version than `current`. Only the dotted
/// numbers count, so a "-beta" or build suffix does not make a version newer.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |text: &str| {
        text.split(['-', '+'])
            .next()
            .unwrap_or(text)
            .split('.')
            .map(|part| part.trim().parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let (candidate, current) = (parts(candidate), parts(current));
    for index in 0..candidate.len().max(current.len()) {
        let left = candidate.get(index).copied().unwrap_or(0);
        let right = current.get(index).copied().unwrap_or(0);
        if left != right {
            return left > right;
        }
    }
    false
}

/// Asks the release host what the newest version is. `None` means this build is it.
pub fn check() -> Result<Option<Manifest>, String> {
    let address = std::env::var(MANIFEST_URL_ENV).unwrap_or_else(|_| MANIFEST_URL.to_string());
    let body = fetch(&address, MANIFEST_LIMIT)?;
    let text =
        String::from_utf8(body).map_err(|_| "the release manifest is not text".to_string())?;
    let manifest = Manifest::parse(&text)?;
    Ok(is_newer(&manifest.version, current()).then_some(manifest))
}

/// Downloads the installer for `manifest` into the temp folder and checks its digest.
pub fn download(manifest: &Manifest) -> Result<PathBuf, String> {
    let body = fetch(&manifest.url, INSTALLER_LIMIT)?;
    let file = std::env::temp_dir().join("better-search-setup.exe");
    std::fs::write(&file, &body).map_err(|err| format!("could not save the download: {err}"))?;
    let digest = sha256(&file)?;
    if !digest.eq_ignore_ascii_case(&manifest.sha256) {
        let _ = std::fs::remove_file(&file);
        return Err(
            "the download does not match the published checksum, so nothing was installed".into(),
        );
    }
    Ok(file)
}

/// Runs a downloaded installer with the administrator rights it needs. It replaces the
/// installed copy in place; the window keeps running the old code until it restarts.
pub fn install(hwnd: HWND, installer: &Path) -> Result<(), String> {
    let file = wide(&installer.to_string_lossy());
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // SAFETY: the struct is initialised with its size and NUL-terminated strings.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(format!(
            "could not start the installer: {}",
            std::io::Error::last_os_error()
        ));
    }
    if !info.hProcess.is_null() {
        let mut code = 0u32;
        // SAFETY: the process handle is owned by this call and used before closing.
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hProcess);
        }
        if code != 0 {
            return Err("the installer reported a problem and did not finish".into());
        }
    }
    Ok(())
}

/// An owned WinHTTP handle, closed on every path out of `fetch`.
struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from WinHTTP and is closed once.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn network_error(context: &str) -> String {
    format!("{context}: {}", std::io::Error::last_os_error())
}

/// A small HTTPS GET, for the manifest or the installer.
fn fetch(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let (secure, host, port, target) = split(url)?;
    let agent = wide(AGENT);
    // SAFETY: NUL-terminated agent; the automatic proxy setting uses the system proxy.
    let session = Handle(unsafe {
        WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            null(),
            null(),
            0,
        )
    });
    if session.0.is_null() {
        return Err(network_error("could not reach the update service"));
    }
    // SAFETY: the session handle is live for the duration of the call.
    unsafe { WinHttpSetTimeouts(session.0, 10_000, 10_000, 20_000, 20_000) };
    let host = wide(&host);
    // SAFETY: session is live and `host` is NUL-terminated.
    let connect = Handle(unsafe { WinHttpConnect(session.0, host.as_ptr(), port, 0) });
    if connect.0.is_null() {
        return Err(network_error("could not connect to the update service"));
    }
    let verb = wide("GET");
    let target = wide(&target);
    // SAFETY: connect is live; verb and target are NUL-terminated; no extra headers.
    let request = Handle(unsafe {
        WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            target.as_ptr(),
            null(),
            null(),
            null(),
            if secure { WINHTTP_FLAG_SECURE } else { 0 },
        )
    });
    if request.0.is_null() {
        return Err(network_error("could not build the update request"));
    }
    // SAFETY: request is live; null optional body and headers.
    if unsafe { WinHttpSendRequest(request.0, null(), 0, null(), 0, 0, 0) } == 0 {
        return Err(network_error("the update request failed"));
    }
    // SAFETY: request is live; no reserved argument.
    if unsafe { WinHttpReceiveResponse(request.0, null_mut()) } == 0 {
        return Err(network_error("the update service did not answer"));
    }
    let status = status_code(request.0)?;
    if status != 200 {
        return Err(format!("the update service answered HTTP {status}"));
    }
    let mut body = Vec::new();
    loop {
        let mut available = 0u32;
        // SAFETY: request is live and `available` is a valid out parameter.
        if unsafe { WinHttpQueryDataAvailable(request.0, &mut available) } == 0 {
            return Err(network_error("the download stopped early"));
        }
        if available == 0 {
            return Ok(body);
        }
        let mut chunk = vec![0u8; available as usize];
        let mut read = 0u32;
        // SAFETY: the buffer is `available` bytes and the count is written here.
        if unsafe { WinHttpReadData(request.0, chunk.as_mut_ptr().cast(), available, &mut read) }
            == 0
        {
            return Err(network_error("the download stopped early"));
        }
        if read == 0 {
            return Ok(body);
        }
        chunk.truncate(read as usize);
        body.extend_from_slice(&chunk);
        if body.len() > limit {
            return Err("the download is much larger than expected".into());
        }
    }
}

fn status_code(request: *mut core::ffi::c_void) -> Result<u32, String> {
    let mut status = 0u32;
    let mut size = size_of::<u32>() as u32;
    let mut index = 0u32;
    // SAFETY: request is live; the buffer is a u32 as the number flag requires.
    let ok = unsafe {
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            (&raw mut status).cast(),
            &mut size,
            &mut index,
        )
    };
    if ok == 0 {
        return Err(network_error("the update service sent no answer to read"));
    }
    Ok(status)
}

/// Splits a web address into whether it is secure, its host, its port and its path.
fn split(url: &str) -> Result<(bool, String, u16, String), String> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err("the update address is not a web address".into());
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => rest.split_at(at),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .map_err(|_| "the update address has a bad port".to_string())?,
        ),
        None => (authority, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err("the update address has no host".into());
    }
    if !secure {
        return Err("the update address is not secure".into());
    }
    Ok((secure, host.to_string(), port, path.to_string()))
}

/// The SHA-256 of a file, as lowercase hex.
fn sha256(file: &Path) -> Result<String, String> {
    use std::io::Read;

    let mut algorithm: windows_sys::Win32::Security::Cryptography::BCRYPT_ALG_HANDLE = null_mut();
    // SAFETY: the algorithm identifier is a constant; the handle is freed below.
    if unsafe { BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_SHA256_ALGORITHM, null(), 0) }
        < 0
    {
        return Err("could not check the download's checksum".into());
    }
    let property = |name, algorithm| -> Result<u32, String> {
        let mut value = 0u32;
        let mut written = 0u32;
        // SAFETY: the buffer is a u32 and the handle is live.
        let status = unsafe {
            BCryptGetProperty(
                algorithm,
                name,
                (&raw mut value).cast(),
                size_of::<u32>() as u32,
                &mut written,
                0,
            )
        };
        if status < 0 {
            return Err("could not check the download's checksum".into());
        }
        Ok(value)
    };
    let object = property(BCRYPT_OBJECT_LENGTH, algorithm)?;
    let digest = property(BCRYPT_HASH_LENGTH, algorithm)?;
    let mut object_buffer = vec![0u8; object as usize];
    let mut hash: windows_sys::Win32::Security::Cryptography::BCRYPT_HASH_HANDLE = null_mut();
    // SAFETY: the object buffer is `object` bytes, as the property reported.
    if unsafe {
        BCryptCreateHash(
            algorithm,
            &mut hash,
            object_buffer.as_mut_ptr(),
            object,
            null(),
            0,
            0,
        )
    } < 0
    {
        // SAFETY: the algorithm handle came from the provider above.
        unsafe { BCryptCloseAlgorithmProvider(algorithm, 0) };
        return Err("could not check the download's checksum".into());
    }
    let outcome = (|| {
        let mut file = std::fs::File::open(file)
            .map_err(|err| format!("could not read the download back: {err}"))?;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|err| format!("could not read the download back: {err}"))?;
            if read == 0 {
                break;
            }
            // SAFETY: the buffer holds `read` bytes and the hash handle is live.
            if unsafe { BCryptHashData(hash, buffer.as_ptr(), read as u32, 0) } < 0 {
                return Err("could not check the download's checksum".into());
            }
        }
        let mut bytes = vec![0u8; digest as usize];
        // SAFETY: the output buffer is `digest` bytes, as the property reported.
        if unsafe { BCryptFinishHash(hash, bytes.as_mut_ptr(), digest, 0) } < 0 {
            return Err("could not check the download's checksum".into());
        }
        Ok(bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>())
    })();
    // SAFETY: both handles came from the calls above and are freed once.
    unsafe {
        BCryptDestroyHash(hash);
        BCryptCloseAlgorithmProvider(algorithm, 0);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "version=1.2.3\nurl=https://example.test/better-search-setup.exe\nsha256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n";

    #[test]
    fn a_manifest_reads_back_its_three_fields() {
        let manifest = Manifest::parse(GOOD).unwrap();
        assert_eq!(manifest.version, "1.2.3");
        assert_eq!(manifest.url, "https://example.test/better-search-setup.exe");
        assert!(manifest.sha256.starts_with("0123456789"));
    }

    #[test]
    fn a_manifest_without_every_field_is_refused() {
        assert!(Manifest::parse("version=1.2.3\n").is_err());
        assert!(Manifest::parse("").is_err());
        let insecure = GOOD.replace("https://", "http://");
        assert!(Manifest::parse(&insecure).is_err());
        let short = "version=1.2.3\nurl=https://example.test/better-search-setup.exe\nsha256=abc\n";
        assert!(Manifest::parse(short).is_err());
        let odd = "version=1.2.3\nurl=https://example.test/x.exe\nsha256=zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n";
        assert!(Manifest::parse(odd).is_err());
    }

    #[test]
    fn only_later_versions_are_offered() {
        assert!(is_newer("1.2.3", "1.2.2"));
        assert!(is_newer("1.3", "1.2.9"));
        assert!(is_newer("2.0.0", "1.9.9"));
        assert!(!is_newer("1.2.3", "1.2.3"));
        assert!(!is_newer("1.2.2", "1.2.3"));
        assert!(!is_newer("1.2.3-beta.1", "1.2.3"));
    }

    #[test]
    fn web_addresses_split_into_host_port_and_path() {
        let (secure, host, port, path) = split("https://github.com/a/b/releases/latest").unwrap();
        assert!(secure);
        assert_eq!(host, "github.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/a/b/releases/latest");
        let (_, host, port, path) = split("https://example.test:8443/x").unwrap();
        assert_eq!(
            (host.as_str(), port, path.as_str()),
            ("example.test", 8443, "/x")
        );
        assert!(split("ftp://example.test/x").is_err());
        assert!(split("http://example.test/x").is_err());
    }

    /// The digest of an empty and a known file, checked against the published values.
    #[test]
    fn sha256_matches_the_standard_vectors() {
        let dir = std::env::temp_dir().join(format!("bs-digest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let empty = dir.join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            sha256(&empty).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let abc = dir.join("abc");
        std::fs::write(&abc, b"abc").unwrap();
        assert_eq!(
            sha256(&abc).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Longer than the 64 KiB read buffer, to cover the streaming path.
        let large = dir.join("large");
        std::fs::write(&large, vec![0u8; 200_000]).unwrap();
        assert_eq!(sha256(&large).unwrap().len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
