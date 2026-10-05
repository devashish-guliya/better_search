//! Win+S opens this window instead of Windows search. Explorer reserves the
//! combination, so it cannot be registered as a hotkey; a low-level keyboard hook sees
//! it first and swallows it. When an Explorer window is in front, the search is
//! scoped to the folder it shows. Letters typed while Start is open come here too, so
//! Start works as before with this window as its search.

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess,
};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, KEYEVENTF_KEYUP, VK_CAPITAL, VK_CONTROL, VK_ESCAPE, VK_LWIN,
    VK_MENU, VK_RWIN, VK_SHIFT, keybd_event,
};
use windows_sys::Win32::UI::Shell::SHGetPathFromIDListW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, EVENT_SYSTEM_FOREGROUND, FindWindowExW, GA_ROOT, GetAncestor, GetClassNameW,
    GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HHOOK, KBDLLHOOKSTRUCT, MSG,
    PostMessageW, SetWindowsHookExW, WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT, WM_APP, WM_KEYDOWN,
    WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use windows_sys::core::{GUID, HRESULT};

/// Posted to the window when Win+S was pressed.
pub const WM_WIN_S: u32 = WM_APP + 5;
/// Posted with a character (wParam) typed while Start was open.
pub const WM_START_TYPED: u32 = WM_APP + 6;

static TARGET: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Windows search is turned off, so a SearchHost window that still comes to the
/// front is closed.
static SEARCH_OFF: AtomicBool = AtomicBool::new(false);
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
static SWALLOWED_S: AtomicBool = AtomicBool::new(false);
/// The foreground window last checked by [`start_in_front`], and whether it is Start.
static LAST_FRONT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static LAST_FRONT_IS_START: AtomicBool = AtomicBool::new(false);
/// A virtual key no keyboard has. Pressing it while Win is down tells the shell the
/// Win key was part of a combination, so releasing it does not open Start.
const VK_UNASSIGNED: u8 = 0xE8;

/// Installs the hooks on a thread of their own. Windows silently removes a low-level
/// hook that answers too slowly, so it must not wait behind the window's own work.
pub fn install(target: HWND, enabled: bool, search_off: bool) {
    TARGET.store(target, Ordering::Relaxed);
    ENABLED.store(enabled, Ordering::Relaxed);
    SEARCH_OFF.store(search_off, Ordering::Relaxed);
    let _ = std::thread::Builder::new()
        .name("window-win-s".into())
        .spawn(|| {
            let module = unsafe { GetModuleHandleW(null()) };
            let hook: HHOOK = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module, 0) };
            if hook.is_null() {
                return;
            }
            let _: HWINEVENTHOOK = unsafe {
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    null_mut(),
                    Some(foreground_changed),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                )
            };
            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {}
        });
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn set_search_off(off: bool) {
    SEARCH_OFF.store(off, Ordering::Relaxed);
}

/// The file name of the program that owns `window`, lowercased.
fn program_of(window: HWND, access: u32) -> Option<(String, isize)> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | access, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut buffer = [0u16; 512];
    let mut len = buffer.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut len)
    };
    let path = String::from_utf16_lossy(&buffer[..len as usize]);
    let name = path.rsplit('\\').next().unwrap_or_default().to_lowercase();
    (ok != 0).then_some((name, process as isize)).or_else(|| {
        unsafe { CloseHandle(process) };
        None
    })
}

unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    window: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let search_off = SEARCH_OFF.load(Ordering::Relaxed);
    let access = if search_off { PROCESS_TERMINATE } else { 0 };
    let Some((name, process)) = program_of(window, access) else {
        return;
    };
    if name == "searchhost.exe" && search_off && ENABLED.load(Ordering::Relaxed) {
        unsafe {
            TerminateProcess(process as _, 0);
            PostMessageW(TARGET.load(Ordering::Relaxed), WM_WIN_S, 0, 0);
        }
    }
    unsafe { CloseHandle(process as _) };
}

/// Whether Start is the foreground window. Checked at the key press: the foreground
/// event arrives too late for the first letters typed after opening Start. The
/// answer is kept per window, so typing elsewhere costs one comparison.
fn start_in_front() -> bool {
    let front = unsafe { GetForegroundWindow() };
    if front != LAST_FRONT.swap(front, Ordering::Relaxed) {
        LAST_FRONT_IS_START.store(is_start(front), Ordering::Relaxed);
    }
    LAST_FRONT_IS_START.load(Ordering::Relaxed)
}

/// Taps a key no keyboard has. Windows lets the program that sent the last input take
/// the foreground; it also keeps a released Win key from opening Start.
pub fn tap_unassigned_key() {
    unsafe {
        keybd_event(VK_UNASSIGNED, 0, 0, 0);
        keybd_event(VK_UNASSIGNED, 0, KEYEVENTF_KEYUP, 0);
    }
}

pub fn is_start(window: HWND) -> bool {
    program_of(window, 0).is_some_and(|(name, process)| {
        unsafe { CloseHandle(process as _) };
        name == "startmenuexperiencehost.exe"
    })
}

/// Closes Start, which no other program may take the foreground from. Only call this
/// right after [`is_start`] said Start is in front, or the key reaches another app.
/// Returns once Start has left the front, so the key cannot reach the caller's window.
pub fn close_start() {
    unsafe {
        keybd_event(VK_ESCAPE as u8, 0, 0, 0);
        keybd_event(VK_ESCAPE as u8, 0, KEYEVENTF_KEYUP, 0);
    }
    for _ in 0..50 {
        if !is_start(unsafe { GetForegroundWindow() }) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The character a letter or digit key types, or `None` for other keys.
fn typed_char(vk: u16) -> Option<char> {
    let shift = unsafe { GetAsyncKeyState(i32::from(VK_SHIFT)) } < 0;
    let caps = unsafe { GetKeyState(i32::from(VK_CAPITAL)) } & 1 != 0;
    match vk as u8 {
        letter @ b'A'..=b'Z' if shift != caps => Some(letter as char),
        letter @ b'A'..=b'Z' => Some(letter.to_ascii_lowercase() as char),
        digit @ b'0'..=b'9' if !shift => Some(digit as char),
        _ => None,
    }
}

unsafe extern "system" fn hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
        let down = matches!(w as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(w as u32, WM_KEYUP | WM_SYSKEYUP);
        let vk = key.vkCode as u16;
        let held = |vk: u16| unsafe { GetAsyncKeyState(i32::from(vk)) } < 0;
        // Keys sent by other programs count too, so remapping tools can press Win+S.
        if vk == VK_LWIN || vk == VK_RWIN {
            WIN_DOWN.store(down, Ordering::Relaxed);
        } else if vk == u16::from(b'S') && up && SWALLOWED_S.swap(false, Ordering::Relaxed) {
            return 1;
        } else if vk == u16::from(b'S') && down && WIN_DOWN.load(Ordering::Relaxed) {
            // Win+Shift+S is the screenshot tool and stays with Windows.
            if ENABLED.load(Ordering::Relaxed)
                && !held(VK_SHIFT)
                && !held(VK_CONTROL)
                && !held(VK_MENU)
            {
                SWALLOWED_S.store(true, Ordering::Relaxed);
                tap_unassigned_key();
                unsafe { PostMessageW(TARGET.load(Ordering::Relaxed), WM_WIN_S, 0, 0) };
                return 1;
            }
        } else if down
            && ENABLED.load(Ordering::Relaxed)
            && !WIN_DOWN.load(Ordering::Relaxed)
            && !held(VK_CONTROL)
            && !held(VK_MENU)
            && let Some(c) = typed_char(vk)
            && start_in_front()
        {
            unsafe {
                PostMessageW(
                    TARGET.load(Ordering::Relaxed),
                    WM_START_TYPED,
                    c as usize,
                    0,
                )
            };
            return 1;
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, w, l) }
}

#[repr(C)]
struct Variant {
    vt: u16,
    reserved: [u16; 3],
    value: i64,
    extra: i64,
}

type Release = unsafe extern "system" fn(*mut c_void) -> u32;
type QueryInterface =
    unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;

#[repr(C)]
struct UnknownVtbl {
    query_interface: QueryInterface,
    _add_ref: usize,
    release: Release,
}

#[repr(C)]
struct ShellWindowsVtbl {
    base: UnknownVtbl,
    _dispatch: [usize; 4],
    count: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    item: unsafe extern "system" fn(*mut c_void, Variant, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct ServiceProviderVtbl {
    base: UnknownVtbl,
    query_service: unsafe extern "system" fn(
        *mut c_void,
        *const GUID,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
}

#[repr(C)]
struct ShellBrowserVtbl {
    base: UnknownVtbl,
    get_window: unsafe extern "system" fn(*mut c_void, *mut HWND) -> HRESULT,
    _between: [usize; 11],
    query_active_shell_view: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct FolderViewVtbl {
    base: UnknownVtbl,
    _view_mode: [usize; 2],
    get_folder: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct PersistFolder2Vtbl {
    base: UnknownVtbl,
    _class_and_init: [usize; 2],
    get_cur_folder: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

const CLSID_SHELL_WINDOWS: GUID = GUID::from_u128(0x9ba05972_f6a8_11cf_a442_00a0c90a8f39);
const IID_ISHELL_WINDOWS: GUID = GUID::from_u128(0x85cb6900_4d95_11cf_960c_0080c7f4ee85);
const IID_ISERVICE_PROVIDER: GUID = GUID::from_u128(0x6d5140c1_7436_11ce_8034_00aa006009fa);
const SID_STOP_LEVEL_BROWSER: GUID = GUID::from_u128(0x4c96be40_915c_11cf_99d3_00aa004ae837);
const IID_ISHELL_BROWSER: GUID = GUID::from_u128(0x000214e2_0000_0000_c000_000000000046);
const IID_IFOLDER_VIEW: GUID = GUID::from_u128(0xcde725b0_ccc9_4519_917e_325d72fab4ce);
const IID_IPERSIST_FOLDER2: GUID = GUID::from_u128(0x1ac3d9f0_175c_11d1_95be_00609797ea4f);
const VT_I4: u16 = 3;

/// A COM pointer released when dropped.
struct Com(*mut c_void);

impl Com {
    fn vtbl<T>(&self) -> &T {
        unsafe { &**(self.0 as *const *const T) }
    }

    fn query(&self, iid: &GUID) -> Option<Com> {
        let mut out = null_mut();
        let ok = unsafe { (self.vtbl::<UnknownVtbl>().query_interface)(self.0, iid, &mut out) };
        (ok >= 0 && !out.is_null()).then_some(Com(out))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        unsafe { (self.vtbl::<UnknownVtbl>().release)(self.0) };
    }
}

fn out(f: impl FnOnce(*mut *mut c_void) -> HRESULT) -> Option<Com> {
    let mut ptr = null_mut();
    (f(&mut ptr) >= 0 && !ptr.is_null()).then_some(Com(ptr))
}

fn class_is(hwnd: HWND, name: &str) -> bool {
    let mut buffer = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize]) == name
}

/// The file-system folder an Explorer window shows in its active tab, or `None` for
/// other windows and for virtual folders such as This PC.
pub fn explorer_folder(window: HWND) -> Option<String> {
    if window.is_null() || !class_is(window, "CabinetWClass") {
        return None;
    }
    let tab_class: Vec<u16> = "ShellTabWindowClass".encode_utf16().chain([0]).collect();
    // The active tab is the first one in z-order.
    let active_tab = unsafe { FindWindowExW(window, null_mut(), tab_class.as_ptr(), null()) };
    unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
    let windows = out(|p| unsafe {
        CoCreateInstance(
            &CLSID_SHELL_WINDOWS,
            null_mut(),
            CLSCTX_ALL,
            &IID_ISHELL_WINDOWS,
            p,
        )
    })?;
    let vtbl = windows.vtbl::<ShellWindowsVtbl>();
    let mut count = 0;
    unsafe { (vtbl.count)(windows.0, &mut count) };
    let mut fallback = None;
    for i in 0..count {
        let index = Variant {
            vt: VT_I4,
            reserved: [0; 3],
            value: i64::from(i),
            extra: 0,
        };
        let Some(item) = out(|p| unsafe { (vtbl.item)(windows.0, index, p) }) else {
            continue;
        };
        let Some(provider) = item.query(&IID_ISERVICE_PROVIDER) else {
            continue;
        };
        let Some(browser) = out(|p| unsafe {
            (provider.vtbl::<ServiceProviderVtbl>().query_service)(
                provider.0,
                &SID_STOP_LEVEL_BROWSER,
                &IID_ISHELL_BROWSER,
                p,
            )
        }) else {
            continue;
        };
        let browser_vtbl = browser.vtbl::<ShellBrowserVtbl>();
        let mut tab: HWND = null_mut();
        unsafe { (browser_vtbl.get_window)(browser.0, &mut tab) };
        if tab.is_null() || unsafe { GetAncestor(tab, GA_ROOT) } != window {
            continue;
        }
        let path = (|| {
            let view = out(|p| unsafe { (browser_vtbl.query_active_shell_view)(browser.0, p) })?;
            let folder_view = view.query(&IID_IFOLDER_VIEW)?;
            let persist = out(|p| unsafe {
                (folder_view.vtbl::<FolderViewVtbl>().get_folder)(
                    folder_view.0,
                    &IID_IPERSIST_FOLDER2,
                    p,
                )
            })?;
            let mut pidl = null_mut();
            let ok = unsafe {
                (persist.vtbl::<PersistFolder2Vtbl>().get_cur_folder)(persist.0, &mut pidl)
            };
            if ok < 0 || pidl.is_null() {
                return None;
            }
            let mut buffer = [0u16; 1024];
            let found = unsafe { SHGetPathFromIDListW(pidl as _, buffer.as_mut_ptr()) };
            unsafe { CoTaskMemFree(pidl) };
            let len = buffer.iter().position(|&c| c == 0).unwrap_or(0);
            (found != 0 && len > 0).then(|| String::from_utf16_lossy(&buffer[..len]))
        })();
        if tab == active_tab || active_tab.is_null() {
            return path;
        }
        fallback = fallback.or(path);
    }
    fallback
}
