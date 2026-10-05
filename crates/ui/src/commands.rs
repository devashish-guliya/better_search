//! Windows Settings pages and power commands, offered as results next to files and
//! apps. Settings pages open through their `ms-settings:` URI; power commands have
//! `command:` paths that only this window understands.

use bs_pipe::Hit;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONWARNING, MB_YESNO, MessageBoxW, SW_HIDE,
};

use crate::apps;

const POWER_PREFIX: &str = "command:";
/// Where command rows take their icon from: the Settings app.
pub const ICON_SOURCE: &str = "shell:AppsFolder\\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";
const MAX_RESULTS: usize = 6;
/// Below an app with the same kind of match: someone typing "camera" more likely
/// wants the app than its privacy page.
const BELOW_APPS: i32 = 12;
/// A match on a keyword only, not the page's own name.
const KEYWORD: i32 = 120;

/// Name, extra words people search for, and the target.
const ENTRIES: &[(&str, &str, &str)] = &[
    (
        "Settings",
        "control panel preferences options",
        "ms-settings:",
    ),
    (
        "Display",
        "screen resolution brightness scale monitor hdr refresh rate",
        "ms-settings:display",
    ),
    ("Night light", "blue light warm", "ms-settings:nightlight"),
    (
        "Sound",
        "volume audio speakers headphones output input",
        "ms-settings:sound",
    ),
    (
        "Microphone settings",
        "mic input recording",
        "ms-settings:sound",
    ),
    (
        "Notifications",
        "do not disturb focus alerts",
        "ms-settings:notifications",
    ),
    (
        "Power & battery",
        "sleep battery saver energy screen timeout",
        "ms-settings:powersleep",
    ),
    (
        "Storage",
        "disk space cleanup storage sense drives",
        "ms-settings:storagesense",
    ),
    (
        "Multitasking",
        "snap windows alt tab",
        "ms-settings:multitasking",
    ),
    (
        "Clipboard",
        "clipboard history paste",
        "ms-settings:clipboard",
    ),
    (
        "About",
        "pc name specs version system info rename computer",
        "ms-settings:about",
    ),
    (
        "Bluetooth & devices",
        "bluetooth pair headphones devices",
        "ms-settings:bluetooth",
    ),
    (
        "Printers & scanners",
        "printer scanner print",
        "ms-settings:printers",
    ),
    (
        "Mouse",
        "pointer cursor speed scroll",
        "ms-settings:mousetouchpad",
    ),
    (
        "Touchpad",
        "trackpad gestures",
        "ms-settings:devices-touchpad",
    ),
    (
        "Wi-Fi",
        "wifi wireless network internet",
        "ms-settings:network-wifi",
    ),
    (
        "Network & internet",
        "ethernet internet ip dns",
        "ms-settings:network",
    ),
    ("VPN", "vpn", "ms-settings:network-vpn"),
    ("Proxy", "proxy", "ms-settings:network-proxy"),
    (
        "Mobile hotspot",
        "hotspot tethering share internet",
        "ms-settings:network-mobilehotspot",
    ),
    (
        "Personalization",
        "customize appearance",
        "ms-settings:personalization",
    ),
    (
        "Background",
        "wallpaper desktop picture",
        "ms-settings:personalization-background",
    ),
    (
        "Colors",
        "dark mode light mode accent color theme transparency",
        "ms-settings:colors",
    ),
    ("Themes", "theme desktop icons", "ms-settings:themes"),
    (
        "Lock screen",
        "lock screen picture screensaver",
        "ms-settings:lockscreen",
    ),
    (
        "Start",
        "start menu pinned recommended",
        "ms-settings:personalization-start",
    ),
    (
        "Taskbar",
        "taskbar widgets system tray",
        "ms-settings:taskbar",
    ),
    ("Fonts", "font typeface", "ms-settings:fonts"),
    (
        "Installed apps",
        "uninstall remove programs apps features",
        "ms-settings:appsfeatures",
    ),
    (
        "Default apps",
        "open with file associations browser",
        "ms-settings:defaultapps",
    ),
    (
        "Startup apps",
        "startup autostart boot",
        "ms-settings:startupapps",
    ),
    (
        "Optional features",
        "features add",
        "ms-settings:optionalfeatures",
    ),
    (
        "Your info",
        "account profile picture",
        "ms-settings:yourinfo",
    ),
    (
        "Email & accounts",
        "email accounts mail",
        "ms-settings:emailandaccounts",
    ),
    (
        "Sign-in options",
        "password pin windows hello fingerprint face login",
        "ms-settings:signinoptions",
    ),
    (
        "Date & time",
        "clock time zone timezone date",
        "ms-settings:dateandtime",
    ),
    (
        "Language & region",
        "language region keyboard layout input locale",
        "ms-settings:regionlanguage",
    ),
    (
        "Typing",
        "autocorrect spelling touch keyboard",
        "ms-settings:typing",
    ),
    (
        "Game Mode",
        "gaming games game bar",
        "ms-settings:gaming-gamemode",
    ),
    (
        "Accessibility",
        "ease of access narrator magnifier contrast text size",
        "ms-settings:easeofaccess",
    ),
    (
        "Privacy & security",
        "privacy permissions security",
        "ms-settings:privacy",
    ),
    (
        "Camera privacy",
        "webcam camera access",
        "ms-settings:privacy-webcam",
    ),
    (
        "Microphone privacy",
        "microphone mic access",
        "ms-settings:privacy-microphone",
    ),
    ("Location", "location gps", "ms-settings:privacy-location"),
    (
        "Windows Security",
        "antivirus defender firewall virus",
        "windowsdefender:",
    ),
    (
        "Windows Update",
        "updates update upgrade patch",
        "ms-settings:windowsupdate",
    ),
    (
        "Recovery",
        "reset this pc restore advanced startup",
        "ms-settings:recovery",
    ),
    (
        "Activation",
        "product key license activate",
        "ms-settings:activation",
    ),
    (
        "For developers",
        "developer mode sudo",
        "ms-settings:developers",
    ),
    (
        "Shut down",
        "shutdown power off turn off",
        "command:shutdown",
    ),
    ("Restart", "reboot restart", "command:restart"),
    ("Sleep", "suspend standby", "command:sleep"),
    ("Lock", "lock computer lock pc", "command:lock"),
    (
        "Sign out",
        "log off logoff logout sign out",
        "command:signout",
    ),
];

pub fn is_command(path: &str) -> bool {
    ENTRIES.iter().any(|&(_, _, target)| target == path)
}

pub fn is_power(path: &str) -> bool {
    path.starts_with(POWER_PREFIX)
}

/// Settings pages and commands matching `query`, best first.
pub fn matching(query: &str) -> Vec<(Hit, String)> {
    let query = query.trim().to_lowercase();
    let terms: Vec<&str> = query.split_whitespace().collect();
    if terms.is_empty() || terms.len() > 6 || query.contains([':', '\\', '/', '*', '?', '"']) {
        return Vec::new();
    }
    let mut found: Vec<(Hit, String)> = ENTRIES
        .iter()
        .filter_map(|&(name, keywords, target)| {
            let lower = name.to_lowercase();
            let score = terms
                .iter()
                .map(|term| {
                    apps::term_score(&lower, term).or_else(|| {
                        // Keywords count from their start: "vol" finds Sound by "volume".
                        (term.len() >= 3 && keywords.split(' ').any(|word| word.starts_with(term)))
                            .then_some(KEYWORD)
                    })
                })
                .try_fold(i32::MAX, |low, s| s.map(|s| low.min(s)))?;
            let hit = Hit {
                path: target.into(),
                is_dir: false,
                score: score - BELOW_APPS,
            };
            Some((hit, name.to_owned()))
        })
        .collect();
    found.sort_by(|a, b| b.0.score.cmp(&a.0.score).then_with(|| a.1.cmp(&b.1)));
    // Two names for one page ("Sound", "Microphone settings"): keep the better one.
    let mut seen = std::collections::HashSet::new();
    found.retain(|(hit, _)| seen.insert(hit.path.clone()));
    found.truncate(MAX_RESULTS);
    found
}

#[link(name = "user32")]
unsafe extern "system" {
    fn LockWorkStation() -> i32;
}

#[link(name = "powrprof")]
unsafe extern "system" {
    fn SetSuspendState(hibernate: u8, force: u8, wake_events_disabled: u8) -> u8;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Runs a `command:` path. Ending the session asks first, so a stray Enter does not
/// throw away unsaved work.
pub fn run(hwnd: HWND, path: &str) {
    let ask = |what: &str| {
        let text = wide(&format!(
            "{what} now? Unsaved work in open apps may be lost."
        ));
        let title = wide("better_search");
        let answer = unsafe {
            MessageBoxW(
                hwnd,
                text.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONWARNING,
            )
        };
        answer == IDYES
    };
    let shutdown = |args: &str| {
        let file = wide("shutdown.exe");
        let args = wide(args);
        unsafe {
            ShellExecuteW(
                hwnd,
                std::ptr::null(),
                file.as_ptr(),
                args.as_ptr(),
                std::ptr::null(),
                SW_HIDE,
            )
        };
    };
    match path.strip_prefix(POWER_PREFIX).unwrap_or_default() {
        "shutdown" if ask("Shut down") => shutdown("/s /hybrid /t 0"),
        "restart" if ask("Restart") => shutdown("/r /t 0"),
        "signout" if ask("Sign out") => shutdown("/l"),
        "lock" => unsafe {
            LockWorkStation();
        },
        "sleep" => unsafe {
            SetSuspendState(0, 0, 0);
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(query: &str) -> Vec<String> {
        matching(query).into_iter().map(|(_, name)| name).collect()
    }

    #[test]
    fn finds_pages_by_name_and_keyword() {
        assert_eq!(names("display")[0], "Display");
        assert_eq!(names("wallpaper"), vec!["Background"]);
        assert!(names("vol").contains(&"Sound".to_string()));
        assert_eq!(names("restart")[0], "Restart");
        assert_eq!(names("uninstall"), vec!["Installed apps"]);
        // Short keyword fragments and file-style queries match nothing.
        assert!(names("vo").is_empty());
        assert!(names("ext:pdf").is_empty());
        assert!(is_command("ms-settings:display") && is_power("command:lock"));
        assert!(!is_command(r"C:\Users\a\display.txt"));
    }
}
