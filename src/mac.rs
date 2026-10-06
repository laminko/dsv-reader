//! The macOS parts gpui does not cover. gpui's delegate gives Finder's files to `on_open_urls`,
//! but it has no `applicationShouldTerminate:`: ⌘Q, the Dock's Quit, and a log out would skip
//! the question about an unsaved column order. This adds that method to gpui's delegate class,
//! as the egui app did to winit's. It also gives the bare binary its Dock icon.
//! Plain libobjc calls: no new crate.

use std::cell::RefCell;
use std::ffi::{CStr, OsString, c_char, c_void};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn object_getClass(obj: Id) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn class_addMethod(class: Id, sel: Sel, imp: *const c_void, types: *const c_char) -> bool;
    fn objc_msgSend();
}

/// Paths that Finder asked the app to open, oldest first. The window takes them each frame.
pub static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// Set by the window each frame: the column order is not saved, so a quit asks first.
pub static MODIFIED: AtomicBool = AtomicBool::new(false);
/// Set when a quit was stopped; the window then asks.
pub static ASK_QUIT: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Makes the window draw a frame, which takes OPENED and ASK_QUIT. Set once the window exists.
    static WAKE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

pub fn set_wake(f: impl Fn() + 'static) {
    WAKE.with(|w| *w.borrow_mut() = Some(Box::new(f)));
}

pub fn wake() {
    WAKE.with(|w| {
        if let Some(f) = &*w.borrow() {
            f();
        }
    });
}

/// A file URL as AppKit gives it ("file:///a%20b.csv") as a path. Other URLs: None.
pub fn file_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let b = rest.strip_prefix("localhost").unwrap_or(rest).as_bytes();
    let hex = |i: usize| b.get(i).and_then(|&c| (c as char).to_digit(16));
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        match (b[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => (out.push((h * 16 + l) as u8), i += 3),
            (c, _, _) => (out.push(c), i += 1),
        };
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

unsafe fn sel(name: &CStr) -> Sel {
    unsafe { sel_registerName(name.as_ptr()) }
}

unsafe fn class(name: &CStr) -> Id {
    unsafe { objc_getClass(name.as_ptr()) }
}

/// `objc_msgSend` as a function of the type `F`. On arm64 a call must use the exact signature.
unsafe fn send<F: Copy>() -> F {
    unsafe { std::mem::transmute_copy(&(objc_msgSend as unsafe extern "C" fn())) }
}

/// `-[delegate applicationShouldTerminate:]`: with an unsaved column order, stop the quit and
/// let the window ask (0 is NSTerminateCancel). Else exit now: AppKit's own quit closes the
/// window, and the Touch Bar finder then aborts the app (see `main`).
extern "C" fn should_terminate(_this: Id, _cmd: Sel, _app: Id) -> usize {
    if !MODIFIED.load(Ordering::Relaxed) {
        std::process::exit(0);
    }
    ASK_QUIT.store(true, Ordering::Relaxed);
    wake();
    0
}

/// Adds `should_terminate` to gpui's delegate class, and gives a bare binary the Dock icon
/// (a bundle has AppIcon.icns). Call once the app has finished launching.
pub fn install() {
    unsafe {
        let get: unsafe extern "C" fn(Id, Sel) -> Id = send();
        let set: unsafe extern "C" fn(Id, Sel, Id) = send();
        let app = get(class(c"NSApplication"), sel(c"sharedApplication"));
        let delegate = get(app, sel(c"delegate"));
        if !delegate.is_null() {
            let quit = should_terminate as extern "C" fn(Id, Sel, Id) -> usize as *const c_void;
            class_addMethod(object_getClass(delegate), sel(c"applicationShouldTerminate:"), quit, c"Q@:@".as_ptr());
            // AppKit can keep what a delegate answers from when it was set: set it again.
            set(app, sel(c"setDelegate:"), std::ptr::null_mut());
            set(app, sel(c"setDelegate:"), delegate);
        }
        if std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/")) {
            return;
        }
        // ponytail: an NSImage from PNG data; egui issue 7155 saw a stray libpng.dylib crash
        // that path. Decode with gpui's image code if a crash shows.
        static PNG: &[u8] = include_bytes!("../icon.png");
        let bytes: unsafe extern "C" fn(Id, Sel, *const c_void, usize) -> Id = send();
        let init: unsafe extern "C" fn(Id, Sel, Id) -> Id = send();
        let data = bytes(class(c"NSData"), sel(c"dataWithBytes:length:"), PNG.as_ptr().cast(), PNG.len());
        let image = init(get(class(c"NSImage"), sel(c"alloc")), sel(c"initWithData:"), data);
        if !image.is_null() {
            set(app, sel(c"setApplicationIconImage:"), image);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::file_path;
    use std::path::PathBuf;

    #[test]
    fn file_urls_become_paths() {
        assert_eq!(file_path("file:///Users/a/My%20File%E2%9C%93.csv"), Some(PathBuf::from("/Users/a/My File✓.csv")));
        assert_eq!(file_path("file://localhost/tmp/x.tsv"), Some(PathBuf::from("/tmp/x.tsv")));
        assert_eq!(file_path("file:///a%2"), Some(PathBuf::from("/a%2")));
        assert_eq!(file_path("https://example.com/a.csv"), None);
    }
}
