//! Real icons and previews for results: an app's own icon for shortcuts and programs,
//! a thumbnail for photos, videos and documents. The shell makes them with
//! `IShellItemImageFactory`, which can read files and decode images, so it runs on its
//! own thread and the window shows a type icon until the picture arrives.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use windows_sys::Win32::Foundation::{HWND, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
    GetDC, GetDIBits, GetObjectW, HBITMAP, ReleaseDC,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows_sys::Win32::UI::Shell::{SHCreateItemFromParsingName, SIIGBF_RESIZETOFIT};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows_sys::core::{GUID, HRESULT};

pub const WM_THUMBNAIL: u32 = WM_APP + 4;

pub struct Request {
    /// Requests from an older result list are dropped unseen.
    pub generation: u64,
    pub path: String,
    pub size: i32,
}

pub struct Done {
    pub path: String,
    pub size: i32,
    /// A `size` x `size` 32-bit bitmap the receiver must delete, or null.
    pub bitmap: HBITMAP,
}

// SAFETY: the bitmap is a GDI handle owned by whoever holds `Done`; GDI handles are
// process-wide and may be used from any thread.
unsafe impl Send for Done {}

pub fn start(hwnd: HWND, generation: Arc<AtomicU64>) -> (Sender<Request>, Receiver<Done>) {
    let (tx, rx) = mpsc::channel::<Request>();
    let (done_tx, done_rx) = mpsc::channel();
    let target = hwnd as isize;
    thread::Builder::new()
        .name("window-thumbnails".into())
        .spawn(move || {
            // SAFETY: called once on this thread before any COM use.
            unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
            while let Ok(first) = rx.recv() {
                let mut batch = vec![first];
                batch.extend(rx.try_iter());
                for request in batch {
                    if request.generation != generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    let bitmap = picture(&request.path, request.size);
                    let done = Done {
                        path: request.path,
                        size: request.size,
                        bitmap,
                    };
                    if done_tx.send(done).is_err() {
                        return;
                    }
                    // SAFETY: posting to a window handle is safe even if it is gone.
                    unsafe { PostMessageW(target as HWND, WM_THUMBNAIL, 0, 0) };
                }
            }
        })
        .expect("cannot start the thumbnail worker");
    (tx, done_rx)
}

/// `IShellItemImageFactory` has one method after `IUnknown`'s three.
#[repr(C)]
struct FactoryVtbl {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_image: unsafe extern "system" fn(*mut c_void, SIZE, i32, *mut HBITMAP) -> HRESULT,
}

const IID_ISHELLITEMIMAGEFACTORY: GUID = GUID::from_u128(0xbcc18b79_ba16_442f_80c4_8a59c30c463b);

/// The shell's picture of `path`, centred on a transparent square, or null.
fn picture(path: &str, size: i32) -> HBITMAP {
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut factory: *mut c_void = null_mut();
    // SAFETY: valid string and out pointer; the interface is released below.
    let hr = unsafe {
        SHCreateItemFromParsingName(
            wide.as_ptr(),
            null_mut(),
            &IID_ISHELLITEMIMAGEFACTORY,
            &mut factory,
        )
    };
    if hr < 0 || factory.is_null() {
        return null_mut();
    }
    let mut raw: HBITMAP = null_mut();
    // SAFETY: `factory` is a live IShellItemImageFactory whose first field is its vtable.
    let hr = unsafe {
        let vtbl = *(factory as *const *const FactoryVtbl);
        let hr = ((*vtbl).get_image)(
            factory,
            SIZE { cx: size, cy: size },
            SIIGBF_RESIZETOFIT,
            &mut raw,
        );
        ((*vtbl).release)(factory);
        hr
    };
    if hr < 0 || raw.is_null() {
        return null_mut();
    }
    let square = squared(raw, size);
    // SAFETY: the factory's bitmap belongs to us.
    unsafe { DeleteObject(raw) };
    square
}

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative: rows run top to bottom.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Photo thumbnails keep their aspect ratio, but image list slots are square.
fn squared(source: HBITMAP, size: i32) -> HBITMAP {
    let mut info = BITMAP::default();
    // SAFETY: `info` is a BITMAP-sized buffer.
    let ok = unsafe {
        GetObjectW(
            source,
            size_of::<BITMAP>() as i32,
            (&raw mut info).cast::<c_void>(),
        )
    };
    let (width, height) = (info.bmWidth.min(size), info.bmHeight.min(size));
    if ok == 0 || width <= 0 || height <= 0 {
        return null_mut();
    }
    let mut pixels = vec![0u32; (info.bmWidth * info.bmHeight) as usize];
    let mut source_info = bitmap_info(info.bmWidth, info.bmHeight);
    // SAFETY: the buffer holds bmWidth x bmHeight 32-bit pixels.
    let rows = unsafe {
        let dc = GetDC(null_mut());
        let rows = GetDIBits(
            dc,
            source,
            0,
            info.bmHeight as u32,
            pixels.as_mut_ptr().cast::<c_void>(),
            &mut source_info,
            DIB_RGB_COLORS,
        );
        ReleaseDC(null_mut(), dc);
        rows
    };
    if rows == 0 {
        return null_mut();
    }
    // Pictures without alpha are opaque; the padding around them must stay clear.
    if pixels.iter().all(|p| p >> 24 == 0) {
        for p in &mut pixels {
            *p |= 0xff00_0000;
        }
    }
    let target_info = bitmap_info(size, size);
    let mut bits: *mut c_void = null_mut();
    // SAFETY: a fresh DIB section; `bits` points at size x size pixels it owns.
    let target = unsafe {
        CreateDIBSection(
            null_mut(),
            &target_info,
            DIB_RGB_COLORS,
            &mut bits,
            null_mut(),
            0,
        )
    };
    if target.is_null() || bits.is_null() {
        return null_mut();
    }
    // SAFETY: CreateDIBSection zeroes the size x size pixels behind `bits`.
    let out = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u32>(), (size * size) as usize) };
    let (left, top) = ((size - width) / 2, (size - height) / 2);
    for y in 0..height {
        let from = (y * info.bmWidth) as usize;
        let to = ((y + top) * size + left) as usize;
        out[to..to + width as usize].copy_from_slice(&pixels[from..from + width as usize]);
    }
    target
}
