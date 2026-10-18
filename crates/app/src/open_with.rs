//! Files that macOS asks Nimbus to open: Finder's Open With, a
//! double-click when Nimbus is the default app, or a drop on the Dock icon.
//!
//! On macOS these do not arrive as command-line arguments. The system
//! sends the running app an "open these files" message, which `AppKit`
//! hands to the app's *delegate*: an object the app registers to receive
//! application-wide events. With no delegate answering it, `AppKit` tries
//! its own document handling, finds no document classes, and shows
//! "Nimbus cannot open files in the … format".
//!
//! winit 0.30 (and 0.31's betas) do not pass this message on, but winit
//! promises never to set a delegate of its own, so the app may register
//! one, as winit's `platform::macos` docs show. This module is that
//! delegate. It turns each message into an [`AppEvent::OpenFiles`] for the
//! event loop, where the files open like files dropped on the window.
//!
//! An Objective-C class declared from Rust needs `unsafe`: the compiler
//! cannot check that the Rust method matches what `AppKit` will call. If a
//! later winit passes the files on itself, this module can go.
#![expect(
    unsafe_code,
    reason = "declaring an Objective-C class for AppKit to call cannot be checked by the compiler"
)]

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability};
use objc2_app_kit::{NSApplication, NSApplicationDelegate};
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSURL};
use winit::event_loop::EventLoopProxy;

use crate::app::AppEvent;

/// What the delegate carries: the way back into the event loop.
pub struct Ivars {
    proxy: EventLoopProxy<AppEvent>,
}

declare_class!(
    /// The app delegate. Only the open-files message is implemented;
    /// `AppKit` treats every other delegate method as absent.
    pub struct Delegate;

    // SAFETY: NSObject has no subclassing requirements. The class is only
    // used on the main thread (`MainThreadOnly`), which is where AppKit
    // calls delegates. The name is unique to Nimbus.
    unsafe impl ClassType for Delegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "NimbusAppDelegate";
    }

    impl DeclaredClass for Delegate {
        type Ivars = Ivars;
    }

    // SAFETY: NSObjectProtocol has no methods we must provide; NSObject,
    // the superclass, implements them.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the one method below has the selector and the argument
    // types AppKit declares for `application:openURLs:` (an NSApplication
    // and an NSArray of NSURL, returning nothing), as objc2-app-kit's
    // generated `NSApplicationDelegate` binding states.
    unsafe impl NSApplicationDelegate for Delegate {
        #[method(application:openURLs:)]
        fn application_open_urls(&self, _application: &NSApplication, urls: &NSArray<NSURL>) {
            let paths: Vec<PathBuf> = urls.iter().filter_map(file_path).collect();
            log::debug!("open-with: {} path(s) from the system", paths.len());
            if !paths.is_empty() {
                let _ = self.ivars().proxy.send_event(AppEvent::OpenFiles(paths));
            }
        }
    }
);

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

/// Register the delegate with the shared `NSApplication`.
///
/// Call after the event loop is built (winit sets up `NSApplication`
/// there) and before `run_app`, so a file that launched the app is not
/// missed. Keep the returned object alive until `run_app` returns:
/// `AppKit` holds its delegate weakly, so dropping it unregisters it.
/// Returns `None` off the main thread, where `AppKit` cannot be used.
pub fn install(proxy: EventLoopProxy<AppEvent>) -> Option<Retained<Delegate>> {
    let mtm = MainThreadMarker::new()?;
    let this = mtm.alloc::<Delegate>().set_ivars(Ivars { proxy });
    // SAFETY: `this` is a freshly allocated Delegate with its ivars set,
    // and NSObject's `init` returns the initialised object, which is what
    // `msg_send_id!` with `super` and `init` requires.
    let delegate: Retained<Delegate> = unsafe { msg_send_id![super(this), init] };
    let app = NSApplication::sharedApplication(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    Some(delegate)
}
