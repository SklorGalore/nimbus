//! Files that macOS asks Nimbus to open: Finder's Open With, a
//! double-click when Nimbus is the default app, or a drop on the Dock icon.
//!
//! On macOS these do not arrive as command-line arguments. The system
//! sends the running app an "open these files" message
//! (`application:openURLs:`), which `AppKit` hands to the app's
//! *delegate*: an object registered to receive application-wide events.
//! If the delegate has no such method, `AppKit` tries its own document
//! handling, finds no document classes, and shows "Nimbus cannot open
//! files in the … format".
//!
//! winit 0.30 (and 0.31's betas) installs its own delegate, the class
//! `WinitApplicationDelegate`, and panics if it finds another one in its
//! place, so we cannot register our own (despite what winit's
//! `platform::macos` docs say). Instead this module *adds* the one missing
//! method to winit's delegate class at startup. Objective-C allows adding
//! methods to an existing class at run time; winit's own methods are left
//! alone. The method sends an [`AppEvent::OpenFiles`] to the event loop,
//! where the files open like files dropped on the window.
//!
//! Talking to the Objective-C runtime directly needs `unsafe`: the
//! compiler cannot check that our function matches what `AppKit` will
//! call. If a later winit passes the files on itself, this module can go.
#![expect(
    unsafe_code,
    reason = "adding a method to an Objective-C class cannot be checked by the compiler"
)]

use std::path::PathBuf;
use std::sync::Mutex;

use objc2::ffi;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::sel;
use objc2_app_kit::NSApplication;
use objc2_foundation::{MainThreadMarker, NSArray, NSURL};
use winit::event_loop::EventLoopProxy;

use crate::app::AppEvent;

/// The class winit 0.30 registers as the `NSApplication` delegate.
const WINIT_DELEGATE: &str = "WinitApplicationDelegate";

/// The Objective-C type encoding of `application:openURLs:`: returns
/// `void` (`v`), and takes the receiver (`@`), the selector (`:`), then
/// two objects (`@@`), the application and the array of URLs.
const OPEN_URLS_TYPES: &std::ffi::CStr = c"v@:@@";

/// The Rust signature of [`open_urls`], as the runtime will call it.
type OpenUrls = extern "C" fn(&AnyObject, Sel, &NSApplication, &NSArray<NSURL>);

/// Where [`open_urls`] sends what it receives. A plain function added to
/// a class has no Rust state of its own, so the proxy lives here; it is
/// set once in [`install`], before the event loop runs.
static PROXY: Mutex<Option<EventLoopProxy<AppEvent>>> = Mutex::new(None);

/// The method added to winit's delegate class. `AppKit` calls it on the
/// main thread with the files to open.
extern "C" fn open_urls(_this: &AnyObject, _cmd: Sel, _app: &NSApplication, urls: &NSArray<NSURL>) {
    let paths: Vec<PathBuf> = urls.iter().filter_map(file_path).collect();
    log::debug!("open-with: {} path(s) from the system", paths.len());
    if paths.is_empty() {
        return;
    }
    if let Ok(guard) = PROXY.lock()
        && let Some(proxy) = guard.as_ref()
    {
        let _ = proxy.send_event(AppEvent::OpenFiles(paths));
    }
}

/// The file-system path of a `file://` URL, or `None` for any other kind.
fn file_path(url: &NSURL) -> Option<PathBuf> {
    // SAFETY: `url` is a live NSURL borrowed from AppKit's array for this
    // call. `isFileURL` and `path` only read it and have no preconditions;
    // objc2 marks every generated method `unsafe` by default.
    let path = unsafe {
        if !url.isFileURL() {
            return None;
        }
        url.path()?
    };
    // macOS file names are UTF-8, so the string is the path exactly.
    Some(PathBuf::from(path.to_string()))
}

/// Teach winit's application delegate to receive the files Finder sends.
///
/// Call after the event loop is built (that is when winit registers its
/// delegate class and sets the delegate) and before `run_app`, so a file
/// that launched the app is not missed. Does nothing, with a log line, off
/// the main thread or if winit's class is not found (a winit upgrade that
/// renamed it).
pub fn install(proxy: EventLoopProxy<AppEvent>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(class) = AnyClass::get(WINIT_DELEGATE) else {
        log::warn!("open-with: no {WINIT_DELEGATE} class; Finder's Open With will not work");
        return;
    };
    if let Ok(mut guard) = PROXY.lock() {
        *guard = Some(proxy);
    }

    // SAFETY: `Imp` is the untyped form of a method's function pointer;
    // the runtime calls it with the arguments the type encoding promises.
    // `open_urls` takes exactly those (receiver, selector, NSApplication,
    // NSArray of NSURL) and returns nothing, matching `OPEN_URLS_TYPES`
    // and AppKit's declaration of `application:openURLs:`.
    let imp = unsafe { std::mem::transmute::<OpenUrls, Imp>(open_urls) };
    // SAFETY: `class` is a registered class that lives for the whole
    // program (`AnyClass` is a `repr(C)` wrapper of `objc_class`, so the
    // pointer cast is exact). The selector and type encoding are valid,
    // NUL-terminated and static. `class_addMethod` adds the method only if
    // the class has none by that name, so winit's methods are untouched.
    let added = unsafe {
        ffi::class_addMethod(
            std::ptr::from_ref(class)
                .cast::<ffi::objc_class>()
                .cast_mut(),
            sel!(application:openURLs:).as_ptr(),
            Some(imp),
            OPEN_URLS_TYPES.as_ptr(),
        )
    };
    if added == ffi::NO {
        log::warn!("open-with: {WINIT_DELEGATE} already has application:openURLs:");
        return;
    }

    // `NSApplication` notes which methods its delegate has when the
    // delegate is set, so set winit's delegate again for it to see the
    // new one. winit keeps its own reference, so the object stays alive.
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: `delegate` only reads the app's current delegate.
    let delegate = unsafe { app.delegate() };
    if delegate.is_some() {
        app.setDelegate(None);
        app.setDelegate(delegate.as_deref());
    }
}
