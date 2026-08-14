//! Self-capture support for documentation screenshots (macOS only).
//!
//! macOS lets a process capture its own on-screen windows without the
//! Screen Recording TCC permission, so setting `SKERRY_SCREENSHOT=<path>`
//! before launch makes the editor capture its own window once the UI has
//! settled, write the PNG, and exit. Used to regenerate the images on the
//! project website without interactive screen capture.
//!
//! Example: `SKERRY_SCREENSHOT=shot.png skerry src/main.rs`

// This module is the crate's only unsafe code (see lib.rs): direct
// CoreGraphics/ImageIO FFI, no safe wrapper crate available.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use eframe::egui;

#[repr(C)]
#[derive(Copy, Clone)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type CGWindowID = u32;
type CGImageRef = *const c_void;
type CFArrayRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFStringRef = *const c_void;
type CFURLRef = *const c_void;
type CFNumberRef = *const c_void;
type CGImageDestinationRef = *mut c_void;
type CFAllocatorRef = *const c_void;
type CFIndex = isize;

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
const K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW: u32 = 1 << 3;
const K_CG_WINDOW_IMAGE_DEFAULT: u32 = 0;
// kCFNumberSInt32Type
const K_CF_NUMBER_SINT_32_TYPE: CFIndex = 3;
// CGRectNull — with a specific window list this captures the window's
// full bounds.
const CG_RECT_NULL: CGRect = CGRect {
    origin: CGPoint {
        x: f64::INFINITY,
        y: f64::INFINITY,
    },
    size: CGSize {
        width: 0.0,
        height: 0.0,
    },
};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    // Exported CFString constants — using them directly avoids any
    // content-hash mismatch when looking up window-list dictionary keys.
    static kCGWindowNumber: CFStringRef;
    static kCGWindowOwnerPID: CFStringRef;
    static kCGWindowBounds: CFStringRef;

    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: CGWindowID) -> CFArrayRef;
    fn CGWindowListCreateImage(
        screen_bounds: CGRect,
        list_option: u32,
        window_id: CGWindowID,
        image_option: u32,
    ) -> CGImageRef;
    fn CGRectMakeWithDictionaryRepresentation(dict: CFDictionaryRef, rect: *mut CGRect) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: CFIndex) -> *const c_void;
    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: CFStringRef) -> *const c_void;
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFNumberGetValue(number: CFNumberRef, the_type: CFIndex, value_ptr: *mut c_void) -> bool;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFAllocatorRef,
        buffer: *const u8,
        buf_len: CFIndex,
        is_directory: bool,
    ) -> CFURLRef;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationCreateWithURL(
        url: CFURLRef,
        file_type: CFStringRef,
        count: usize,
        options: CFDictionaryRef,
    ) -> CGImageDestinationRef;
    fn CGImageDestinationAddImage(
        dest: CGImageDestinationRef,
        image: CGImageRef,
        properties: CFDictionaryRef,
    );
    fn CGImageDestinationFinalize(dest: CGImageDestinationRef) -> bool;
}

/// Frames to let the UI settle (syntax highlighting, project tree, git
/// gutter) before capturing. Startup frames run unthrottled before the
/// window server maps the window, so the window may not be capturable
/// yet at this point — capture retries every frame until it succeeds.
const SETTLE_FRAMES: u32 = 30;
/// Give up after ~10 seconds of frames.
const MAX_FRAMES: u32 = SETTLE_FRAMES + 600;

static FRAME_COUNT: AtomicU32 = AtomicU32::new(0);
static TARGET: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Called once per frame from `EditorApp::update`. When
/// `SKERRY_SCREENSHOT` is set, keeps the frame loop alive, captures the
/// app's own window after the UI settles, writes the PNG, and exits.
pub fn maybe_capture_and_exit(ctx: &egui::Context) {
    let target = TARGET.get_or_init(|| std::env::var("SKERRY_SCREENSHOT").ok().map(PathBuf::from));
    let Some(path) = target.as_ref() else {
        return;
    };
    // Screenshot mode must keep painting: an idle egui app stops calling
    // update(), which would freeze the capture retry loop.
    ctx.request_repaint();
    let frames = FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
    if frames < SETTLE_FRAMES {
        return;
    }
    if frames > MAX_FRAMES {
        eprintln!("skerry: screenshot failed: window never became capturable");
        std::process::exit(1);
    }
    // Retry each frame until the window is on-screen and captured; report
    // progress roughly once a second so a hang is diagnosable.
    match capture_own_window_to_png(path) {
        Ok(()) => {
            eprintln!("skerry: screenshot saved to {}", path.display());
            std::process::exit(0);
        }
        Err(e) if frames % 60 == 0 => eprintln!("skerry: screenshot retrying: {e}"),
        _ => {}
    }
}

/// Capture this process's largest on-screen window to `path` as PNG.
fn capture_own_window_to_png(path: &Path) -> Result<(), String> {
    let window_id = own_window_id().ok_or("no own window found")?;
    unsafe {
        // Deprecated in favor of ScreenCaptureKit on newer macOS, but
        // self-capture via this API needs no TCC permission.
        #[allow(deprecated)]
        let image = CGWindowListCreateImage(
            CG_RECT_NULL,
            K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
            window_id,
            K_CG_WINDOW_IMAGE_DEFAULT,
        );
        if image.is_null() {
            return Err("CGWindowListCreateImage returned null".into());
        }
        let bytes = path
            .as_os_str()
            .as_encoded_bytes();
        let url = CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as CFIndex,
            false,
        );
        if url.is_null() {
            return Err("CFURLCreateFromFileSystemRepresentation failed".into());
        }
        let png_type = cf_str("public.png");
        let dest = CGImageDestinationCreateWithURL(url, png_type, 1, std::ptr::null());
        if dest.is_null() {
            CFRelease(url);
            CFRelease(png_type);
            return Err("CGImageDestinationCreateWithURL failed".into());
        }
        CGImageDestinationAddImage(dest, image, std::ptr::null());
        let ok = CGImageDestinationFinalize(dest);
        CFRelease(dest);
        CFRelease(png_type);
        CFRelease(url);
        if ok {
            Ok(())
        } else {
            Err("CGImageDestinationFinalize failed".into())
        }
    }
}

/// Find the largest on-screen window owned by this process — the editor
/// viewport rather than any small helper/overlay windows.
fn own_window_id() -> Option<CGWindowID> {
    let pid = std::process::id() as i32;
    unsafe {
        #[allow(deprecated)]
        let list = CGWindowListCopyWindowInfo(K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, 0);
        if list.is_null() {
            return None;
        }
        let mut best: Option<(f64, CGWindowID)> = None;
        let count = CFArrayGetCount(list);
        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(list, i) as CFDictionaryRef;
            if dict.is_null() {
                continue;
            }
            let pid_ref = CFDictionaryGetValue(dict, kCGWindowOwnerPID) as CFNumberRef;
            if pid_ref.is_null() {
                continue;
            }
            let mut owner_pid: i32 = 0;
            if !CFNumberGetValue(
                pid_ref,
                K_CF_NUMBER_SINT_32_TYPE,
                &mut owner_pid as *mut i32 as *mut c_void,
            ) {
                continue;
            }
            if owner_pid != pid {
                continue;
            }
            let bounds_ref = CFDictionaryGetValue(dict, kCGWindowBounds) as CFDictionaryRef;
            if bounds_ref.is_null() {
                continue;
            }
            let mut rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 0.0,
                    height: 0.0,
                },
            };
            if !CGRectMakeWithDictionaryRepresentation(bounds_ref, &mut rect) {
                continue;
            }
            if rect.size.width < 400.0 || rect.size.height < 300.0 {
                continue;
            }
            let id_ref = CFDictionaryGetValue(dict, kCGWindowNumber) as CFNumberRef;
            if id_ref.is_null() {
                continue;
            }
            let mut window_id: i32 = 0;
            if !CFNumberGetValue(
                id_ref,
                K_CF_NUMBER_SINT_32_TYPE,
                &mut window_id as *mut i32 as *mut c_void,
            ) {
                continue;
            }
            let area = rect.size.width * rect.size.height;
            if best.map_or(true, |(a, _)| area > a) {
                best = Some((area, window_id as CGWindowID));
            }
        }
        CFRelease(list);
        best.map(|(_, id)| id)
    }
}

fn cf_str(s: &str) -> CFStringRef {
    // CFStringCreateWithCString reads to the NUL byte — Rust string
    // literals are not NUL-terminated, so the bytes must be copied into
    // a CString first or the created string swallows adjacent memory.
    let c = std::ffi::CString::new(s).expect("no interior NUL");
    unsafe {
        CFStringCreateWithCString(
            std::ptr::null(),
            c.as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        )
    }
}
