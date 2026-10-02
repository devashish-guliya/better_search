//! Per-user, unelevated settings. No service configuration or machine-wide keys.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::ptr::null;

use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::MOD_ALT;

use crate::wide;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "better_search";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub modifiers: u32,
    pub key: u32,
    pub hover: bool,
    pub left: bool,
    pub start_with_windows: bool,
    /// Rank results the user opened before higher (see `frecency`).
    pub history: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            modifiers: MOD_ALT,
            key: u32::from(b' '),
            hover: true,
            left: false,
            start_with_windows: false,
            history: true,
        }
    }
}

fn path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|dir| PathBuf::from(dir).join("better_search").join("window.cfg"))
}

impl Settings {
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                match key {
                    "modifiers" => {
                        if let Ok(n) = value.parse::<u32>() {
                            settings.modifiers = n;
                        }
                    }
                    "key" => {
                        if let Ok(n) = value.parse::<u32>() {
                            settings.key = n;
                        }
                    }
                    "hover" => settings.hover = value == "true",
                    "left" => settings.left = value == "true",
                    "start_with_windows" => settings.start_with_windows = value == "true",
                    "history" => settings.history = value != "false",
                    _ => {}
                }
            }
        }
        // RegisterHotKey rejects a modifier without a key; do not persist a broken hotkey.
        if settings.key == 0
            || settings.key > 255
            || settings.modifiers == 0
            || settings.modifiers & !0x000f != 0
        {
            settings.modifiers = MOD_ALT;
            settings.key = u32::from(b' ');
        }
        settings
    }

    pub fn save(&self) -> io::Result<()> {
        let path = path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA"))?;
        fs::create_dir_all(path.parent().expect("settings filename has a parent"))?;
        let tmp = path.with_extension("tmp");
        fs::write(
            &tmp,
            format!(
                "modifiers={}\nkey={}\nhover={}\nleft={}\nstart_with_windows={}\nhistory={}\n",
                self.modifiers,
                self.key,
                self.hover,
                self.left,
                self.start_with_windows,
                self.history
            ),
        )?;
        fs::rename(tmp, path)
    }

    /// Only the current user's Run key is changed, after the user ticks Save.
    pub fn apply_startup(&self) -> io::Result<()> {
        let key = wide(RUN_KEY);
        let name = wide(VALUE);
        let code = if self.start_with_windows {
            let exe = std::env::current_exe()?;
            let command = wide(&format!("\"{}\" --hidden", exe.display()));
            // SAFETY: terminated strings; data size is in bytes. This is HKCU, never HKLM.
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    name.as_ptr(),
                    REG_SZ,
                    command.as_ptr().cast(),
                    (command.len() * 2) as u32,
                )
            }
        } else {
            // SAFETY: terminated strings; deleting an absent value is harmless.
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) }
        };
        if code == 0 || (!self.start_with_windows && code == ERROR_FILE_NOT_FOUND) {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code as i32))
        }
    }
}

/// Windows app color preference. A missing value defaults to the normal light theme.
pub fn light_theme() -> bool {
    use windows_sys::Win32::System::Registry::{RRF_RT_REG_DWORD, RegGetValueW};
    let mut value = 1u32;
    let mut bytes = size_of::<u32>() as u32;
    // SAFETY: pointers refer to live buffers of the given size.
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize").as_ptr(),
            wide("AppsUseLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            null::<u32>() as *mut u32,
            (&raw mut value).cast(),
            &mut bytes,
        )
    };
    ok != 0 || value != 0
}

/// The service currently indexes all fixed NTFS drives. Display the same eligible
/// drives, but do not pretend this is a per-user filter or let a user reconfigure it.
pub fn indexed_drives() -> String {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };
    let drives = unsafe { GetLogicalDrives() };
    let names: Vec<String> = (0..26)
        .filter(|bit| drives & (1 << bit) != 0)
        .filter_map(|bit| {
            let drive = char::from(b'A' + bit as u8);
            let root = wide(&format!("{drive}:\\"));
            if unsafe { GetDriveTypeW(root.as_ptr()) } != 3 {
                return None;
            }
            let mut fs = [0u16; 32];
            // SAFETY: the root and output buffer are valid; other outputs are optional.
            let ok = unsafe {
                GetVolumeInformationW(
                    root.as_ptr(),
                    null::<u16>() as *mut u16,
                    0,
                    null::<u32>() as *mut u32,
                    null::<u32>() as *mut u32,
                    null::<u32>() as *mut u32,
                    fs.as_mut_ptr(),
                    fs.len() as u32,
                )
            };
            (ok != 0 && String::from_utf16_lossy(&fs[..fs.iter().position(|&c| c == 0)?]) == "NTFS")
                .then(|| format!("{drive}:"))
        })
        .collect();
    if names.is_empty() {
        "None detected".into()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_settings_and_recovers_from_broken_hotkeys() {
        let settings = Settings::parse(
            "modifiers=3\nkey=75\nhover=false\nleft=true\nstart_with_windows=true\n",
        );
        assert_eq!(settings.key, 75);
        assert_eq!(settings.modifiers, 3);
        assert!(!settings.hover);
        assert!(settings.left);
        assert!(settings.start_with_windows);
        assert_eq!(Settings::parse("key=9999").key, u32::from(b' '));
        assert!(settings.history);
        assert!(Settings::parse("").history);
        assert!(!Settings::parse("history=false\n").history);
    }
}
