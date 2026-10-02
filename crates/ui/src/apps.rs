//! Microsoft Store and other packaged (MSIX) apps. They have no shortcut file in the
//! Start Menu folders, so the file index never sees them; the shell lists them in its
//! Applications folder (`shell:AppsFolder`) under an app ID such as
//! `Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`. The window reads that list on a
//! background thread and matches app names itself.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bs_pipe::Hit;
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
use windows_sys::Win32::UI::Shell::SHCreateItemFromParsingName;
use windows_sys::core::{GUID, HRESULT, PWSTR};

/// Paths of packaged-app results. The shell opens them and draws their icons.
pub const PREFIX: &str = "shell:AppsFolder\\";
/// How long a loaded list is trusted before searching reads it again, so newly
/// installed apps turn up without a restart.
const STALE_AFTER: Duration = Duration::from_secs(120);
const MAX_RESULTS: usize = 20;

// Scores on the service's scale, close to what a Start Menu shortcut gets for the
// same kind of match, so both kinds of app mix fairly.
const EXACT: i32 = 180;
const PREFIX_MATCH: i32 = 144;
const WORD_START: i32 = 127;
const SUBSTRING: i32 = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreApp {
    pub name: String,
    lower: String,
    pub path: String,
}

impl StoreApp {
    pub fn new(name: &str, app_id: &str) -> Self {
        Self {
            name: name.to_owned(),
            lower: name.to_lowercase(),
            path: format!("{PREFIX}{app_id}"),
        }
    }
}

#[derive(Default)]
pub struct Apps {
    list: Arc<Mutex<Vec<StoreApp>>>,
    loaded: Option<Instant>,
}

impl Apps {
    /// Reads the app list again in the background if it is missing or old.
    pub fn refresh_if_stale(&mut self) {
        if self.loaded.is_some_and(|at| at.elapsed() < STALE_AFTER) {
            return;
        }
        self.loaded = Some(Instant::now());
        let list = Arc::clone(&self.list);
        let _ = thread::Builder::new()
            .name("window-store-apps".into())
            .spawn(move || {
                let apps = enumerate();
                if !apps.is_empty()
                    && let Ok(mut list) = list.lock()
                {
                    *list = apps;
                }
            });
    }

    /// Packaged apps whose names match `query`, best first.
    pub fn matches(&self, query: &str) -> Vec<(Hit, String)> {
        let Ok(list) = self.list.lock() else {
            return Vec::new();
        };
        matching(&list, query)
    }
}

pub fn matching(apps: &[StoreApp], query: &str) -> Vec<(Hit, String)> {
    let query = query.trim().to_lowercase();
    let terms: Vec<&str> = query.split_whitespace().collect();
    // Filters and paths are file searches.
    if terms.is_empty() || terms.len() > 8 || query.contains([':', '\\', '/', '*', '?', '"']) {
        return Vec::new();
    }
    let mut found: Vec<(Hit, String)> = apps
        .iter()
        .filter_map(|app| {
            let score = if app.lower == query {
                EXACT
            } else {
                terms
                    .iter()
                    .map(|term| term_score(&app.lower, term))
                    .try_fold(i32::MAX, |low, s| s.map(|s| low.min(s)))?
            };
            let hit = Hit {
                path: app.path.clone(),
                is_dir: false,
                score,
            };
            Some((hit, app.name.clone()))
        })
        .collect();
    found.sort_by(|a, b| b.0.score.cmp(&a.0.score).then_with(|| a.1.cmp(&b.1)));
    found.truncate(MAX_RESULTS);
    found
}

fn term_score(name: &str, term: &str) -> Option<i32> {
    // Longer names fit a short term less well.
    let penalty = ((name.len().saturating_sub(term.len())) / 3).min(10) as i32;
    let word_start = name.match_indices(term).any(|(at, _)| {
        at > 0
            && name[..at]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_alphanumeric())
    });
    if name.starts_with(term) {
        Some(PREFIX_MATCH - penalty)
    } else if word_start {
        Some(WORD_START - penalty)
    } else if term.len() >= 2 && name.contains(term) {
        Some(SUBSTRING - penalty)
    } else {
        None
    }
}

#[repr(C)]
struct ItemVtbl {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    bind_to_handler: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const GUID,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    _get_parent: usize,
    get_display_name: unsafe extern "system" fn(*mut c_void, i32, *mut PWSTR) -> HRESULT,
}

#[repr(C)]
struct EnumVtbl {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> HRESULT,
}

const IID_ISHELLITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);
const IID_IENUMSHELLITEMS: GUID = GUID::from_u128(0x70629033_e363_4a28_a567_0db78006e6d7);
const BHID_ENUMITEMS: GUID = GUID::from_u128(0x94f60519_2850_4924_aa5a_d15e84868039);
const SIGDN_NORMALDISPLAY: i32 = 0;
const SIGDN_PARENTRELATIVEPARSING: i32 = 0x8001_8001_u32 as i32;

/// The packaged apps in the shell's Applications folder. Classic apps there come from
/// Start Menu shortcuts the index already has, so only packaged ones (whose app ID
/// has a `!`) are kept.
fn enumerate() -> Vec<StoreApp> {
    let mut apps = Vec::new();
    // SAFETY: COM is initialised once on this short-lived thread; every interface
    // obtained below is released, and every shell string freed with CoTaskMemFree.
    unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let folder: Vec<u16> = "shell:AppsFolder".encode_utf16().chain([0]).collect();
        let mut item: *mut c_void = null_mut();
        if SHCreateItemFromParsingName(folder.as_ptr(), null_mut(), &IID_ISHELLITEM, &mut item) < 0
            || item.is_null()
        {
            return apps;
        }
        let vtbl = *(item as *const *const ItemVtbl);
        let mut items: *mut c_void = null_mut();
        let hr = ((*vtbl).bind_to_handler)(
            item,
            null_mut(),
            &BHID_ENUMITEMS,
            &IID_IENUMSHELLITEMS,
            &mut items,
        );
        ((*vtbl).release)(item);
        if hr < 0 || items.is_null() {
            return apps;
        }
        let list = *(items as *const *const EnumVtbl);
        loop {
            let mut child: *mut c_void = null_mut();
            let mut fetched = 0u32;
            if ((*list).next)(items, 1, &mut child, &mut fetched) != 0 || child.is_null() {
                break;
            }
            let child_vtbl = *(child as *const *const ItemVtbl);
            let text = |kind: i32| {
                let mut raw: PWSTR = null_mut();
                if ((*child_vtbl).get_display_name)(child, kind, &mut raw) < 0 || raw.is_null() {
                    return None;
                }
                let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
                let value = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
                CoTaskMemFree(raw as *const c_void);
                Some(value)
            };
            if let (Some(id), Some(name)) =
                (text(SIGDN_PARENTRELATIVEPARSING), text(SIGDN_NORMALDISPLAY))
                && id.contains('!')
                && !id.contains('\\')
                && !name.trim().is_empty()
            {
                apps.push(StoreApp::new(name.trim(), &id));
            }
            ((*child_vtbl).release)(child);
        }
        ((*list).release)(items);
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_apps_match_by_name() {
        let apps = [
            StoreApp::new(
                "Calculator",
                "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
            ),
            StoreApp::new("Paint", "Microsoft.Paint_8wekyb3d8bbwe!App"),
            StoreApp::new(
                "Sticky Notes",
                "Microsoft.MicrosoftStickyNotes_8wekyb3d8bbwe!App",
            ),
        ];
        let names = |q: &str| -> Vec<String> {
            matching(&apps, q)
                .into_iter()
                .map(|(_, name)| name)
                .collect()
        };
        assert_eq!(names("calc"), ["Calculator"]);
        assert_eq!(names("notes"), ["Sticky Notes"]);
        assert_eq!(names("sticky no"), ["Sticky Notes"]);
        assert!(names("ext:png").is_empty());
        assert!(names("zzz").is_empty());
        let paint = &matching(&apps, "Paint")[0].0;
        assert_eq!(paint.score, EXACT);
        assert_eq!(
            paint.path,
            r"shell:AppsFolder\Microsoft.Paint_8wekyb3d8bbwe!App"
        );
        // A prefix outranks a word start, which outranks a substring.
        let score = |name: &str, term: &str| term_score(name, term).unwrap();
        assert!(score("calculator", "calc") > score("sticky notes", "notes"));
        assert!(score("sticky notes", "notes") > score("onenote", "note"));
    }
}
