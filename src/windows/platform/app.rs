use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, define_class, exception, msg_send};
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString, ns_string};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use rini_core::ids::WindowServerId;
pub use rini_core::ids::pid_t;

use crate::windows::domain::info::{AppInfo, WindowInfo, WindowServerInfo};
use crate::windows::platform::ax::element::{
    AX_STANDARD_WINDOW_SUBROLE, AX_WINDOW_ROLE, AXUIElement, Error as AxError,
};
use crate::windows::platform::window_server::WindowServer;

const NS_KEY_VALUE_OBSERVING_OPTION_NEW: usize = 1 << 0;
const NS_KEY_VALUE_OBSERVING_OPTION_INITIAL: usize = 1 << 2;

type ApplicationCallback = Arc<dyn Fn(pid_t, AppInfo) + Send + Sync + 'static>;

/// The two properties of `NSRunningApplication` rini waits on.
///
/// One value rather than a pair of everything: observe, unobserve and both "already handled"
/// flags existed twice each, differing only in a key path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Observed {
    ActivationPolicy,
    FinishedLaunching,
}

impl Observed {
    const ALL: [Self; 2] = [Self::ActivationPolicy, Self::FinishedLaunching];

    fn key_path(self) -> &'static NSString {
        match self {
            Self::ActivationPolicy => ns_string!("activationPolicy"),
            Self::FinishedLaunching => ns_string!("finishedLaunching"),
        }
    }

    /// Whether the property has reached the value rini is waiting for. A notification fires on
    /// every change, including the ones that are not it yet.
    fn is_ready(self, app: &NSRunningApplication) -> bool {
        match self {
            Self::ActivationPolicy => {
                app.activationPolicy() == NSApplicationActivationPolicy::Regular
            }
            Self::FinishedLaunching => app.isFinishedLaunching(),
        }
    }
}

struct ApplicationObserverIvars {
    app: Retained<NSRunningApplication>,
    handler: ApplicationCallback,
    info: AppInfo,
    pid: pid_t,
    observing: [Cell<bool>; 2],
    notified: [Cell<bool>; 2],
}

impl ApplicationObserverIvars {
    fn observing(&self, what: Observed) -> &Cell<bool> { &self.observing[what as usize] }

    fn notified(&self, what: Observed) -> &Cell<bool> { &self.notified[what as usize] }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = ApplicationObserverIvars]
    struct ApplicationObserver;

    impl ApplicationObserver {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut c_void,
        ) {
            let Some(key_path) = key_path else {
                return;
            };
            for what in Observed::ALL {
                if key_path.isEqualToString(what.key_path()) {
                    self.handle(what);
                    return;
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for ApplicationObserver {}
);

impl ApplicationObserver {
    fn new(
        app: Retained<NSRunningApplication>,
        info: AppInfo,
        handler: ApplicationCallback,
    ) -> Retained<Self> {
        let pid = app.pid();
        let observer = Self::alloc().set_ivars(ApplicationObserverIvars {
            app,
            handler,
            info,
            pid,
            observing: [Cell::new(false), Cell::new(false)],
            notified: [Cell::new(false), Cell::new(false)],
        });
        // SAFETY: `init` on a freshly allocated NSObject subclass with its ivars set returns it.
        unsafe { msg_send![super(observer), init] }
    }

    fn observe(&self, what: Observed) {
        let ivars = self.ivars();
        if ivars.observing(what).get() || ivars.notified(what).get() {
            return;
        }
        ivars.observing(what).set(true);
        // SAFETY: the selector's argument types; KVO keeps `self` unretained, and `Drop` removes it
        // before it is freed.
        unsafe {
            let _: () = msg_send![
                &*ivars.app,
                addObserver: self,
                forKeyPath: what.key_path(),
                options: (NS_KEY_VALUE_OBSERVING_OPTION_NEW | NS_KEY_VALUE_OBSERVING_OPTION_INITIAL),
                context: std::ptr::null_mut::<c_void>()
            ];
        }
    }

    /// Report once, the first time the property is ready, and stop observing it.
    fn handle(&self, what: Observed) {
        let (callback, info, pid) = {
            let ivars = self.ivars();
            if ivars.notified(what).get() || !what.is_ready(&ivars.app) {
                return;
            }
            ivars.notified(what).set(true);
            (ivars.handler.clone(), ivars.info.clone(), ivars.pid)
        };

        self.unobserve(what);
        callback(pid, info);
    }

    fn unobserve(&self, what: Observed) {
        let ivars = self.ivars();
        if !ivars.observing(what).replace(false) {
            return;
        }
        // SAFETY: the selector's argument types, for an observation `observing` says was added.
        let _ = exception::catch(AssertUnwindSafe(|| unsafe {
            let _: () = msg_send![
                &*ivars.app,
                removeObserver: self,
                forKeyPath: what.key_path(),
                context: std::ptr::null_mut::<c_void>()
            ];
        }));
    }
}

impl Drop for ApplicationObserver {
    fn drop(&mut self) {
        for what in Observed::ALL {
            self.unobserve(what);
        }
    }
}

static APPLICATION_CALLBACK: Lazy<Mutex<Option<ApplicationCallback>>> =
    Lazy::new(|| Mutex::new(None));

thread_local! {
    /// The observer of each application waited on, on the thread that asks: the main thread. Never
    /// dropped, so no observer messages AppKit from a thread-exit destructor.
    static APPLICATION_OBSERVERS: ManuallyDrop<RefCell<HashMap<pid_t, Retained<ApplicationObserver>>>> =
        ManuallyDrop::new(RefCell::new(HashMap::new()));
}

pub fn set_application_callback<F>(callback: F)
where F: Fn(pid_t, AppInfo) + Send + Sync + 'static {
    *APPLICATION_CALLBACK.lock() = Some(Arc::new(callback));
}

/// Wait for the application to settle on an activation policy, then report it.
///
/// A vanished process still gets the callback: the question was what kind of application this is,
/// and rini already has the answer it is going to get. The caller needs to hear back either way so
/// the pending launch does not sit there forever.
pub fn ensure_activation_policy_observer(pid: pid_t, info: AppInfo) {
    let callback = APPLICATION_CALLBACK.lock().clone();
    let Some(callback) = callback else {
        return;
    };
    let Some(app) = NSRunningApplication::with_process_id(pid) else {
        callback(pid, info);
        return;
    };
    observe_application(app, info, callback, |observer| {
        observer.observe(Observed::ActivationPolicy)
    });
}

/// Wait for the application to finish launching, then report it.
///
/// Deliberately not symmetrical with `ensure_activation_policy_observer`. A vanished process gets NO
/// callback here, because the event being waited for is a launch and a process that is gone will
/// never finish one; reporting it would announce a launch that did not happen. An application that
/// has ALREADY finished launching is reported at once rather than observed, since the notification
/// it would wait for has been and gone.
pub fn ensure_finished_launching_observer(pid: pid_t, info: AppInfo) {
    let callback = APPLICATION_CALLBACK.lock().clone();
    let Some(callback) = callback else {
        return;
    };
    let Some(app) = NSRunningApplication::with_process_id(pid) else {
        return;
    };
    if app.isFinishedLaunching() {
        callback(pid, info);
        return;
    };
    observe_application(app, info, callback, |observer| {
        observer.observe(Observed::FinishedLaunching)
    });
}

pub fn remove_application_observer(pid: pid_t) {
    let observer = APPLICATION_OBSERVERS.with(|observers| observers.borrow_mut().remove(&pid));
    drop(observer);
}

fn observe_application(
    app: Retained<NSRunningApplication>,
    info: AppInfo,
    callback: ApplicationCallback,
    observe: impl FnOnce(&ApplicationObserver),
) {
    let pid = app.pid();
    let observer = APPLICATION_OBSERVERS.with(|observers| {
        let mut observers = observers.borrow_mut();
        let observer = observers
            .entry(pid)
            .or_insert_with(|| ApplicationObserver::new(app, info, callback));
        Retained::clone(observer)
    });
    observe(&observer);
}

pub fn running_apps() -> impl Iterator<Item = (pid_t, AppInfo)> {
    let callback = APPLICATION_CALLBACK.lock().clone();
    NSWorkspace::sharedWorkspace()
        .runningApplications()
        .into_iter()
        .filter_map(move |app| {
            let bundle_id = app.bundle_id().as_ref().map(|b| b.to_string());

            let info = AppInfo::from(&*app);
            let pid = app.pid();

            if app.activationPolicy() != NSApplicationActivationPolicy::Regular
                && bundle_id.as_deref() != Some("com.apple.loginwindow")
            {
                if let Some(cb) = callback.clone() {
                    observe_application(app, info, cb, |observer| {
                        observer.observe(Observed::ActivationPolicy)
                    });
                }
                return None;
            }

            Some((pid, info))
        })
}

/// Check one bundle identity without producing a process inventory.
///
/// Persistence uses this only to decide whether a stale saved window still has a running
/// application whose AX discovery may provide a fuzzy match.
pub fn is_bundle_running(bundle_id: &str) -> bool {
    !NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle_id))
        .is_empty()
}

pub trait NSRunningApplicationExt {
    fn with_process_id(pid: pid_t) -> Option<Retained<Self>>;
    fn pid(&self) -> pid_t;
    fn bundle_id(&self) -> Option<Retained<NSString>>;
    fn localized_name(&self) -> Option<Retained<NSString>>;
    /// The application's icon, as a `CGImage` a layer can draw; see `image_pixels`.
    ///
    /// `None` for an application with no icon and for one that has quit between being listed and being
    /// asked — the second is ordinary, since a switcher list is a moment old by the time it is drawn.
    fn icon_image(&self, size: f64) -> Option<Retained<objc2_core_graphics::CGImage>>;
}

impl NSRunningApplicationExt for NSRunningApplication {
    fn with_process_id(pid: pid_t) -> Option<Retained<Self>> {
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
    }

    fn pid(&self) -> pid_t { self.processIdentifier() }

    fn bundle_id(&self) -> Option<Retained<NSString>> { self.bundleIdentifier() }

    fn localized_name(&self) -> Option<Retained<NSString>> { self.localizedName() }

    fn icon_image(&self, size: f64) -> Option<Retained<objc2_core_graphics::CGImage>> {
        image_pixels(&*self.icon()?, size)
    }
}

/// An image as pixels a layer can draw, at the best representation for `size` points square.
///
/// An `NSImage` is a description of how to draw at some size, and a layer wants pixels.
pub fn image_pixels(
    image: &objc2_app_kit::NSImage,
    size: f64,
) -> Option<Retained<objc2_core_graphics::CGImage>> {
    let mut rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size, size));
    // SAFETY: `rect` is a local, not a temporary, because AppKit writes the rect it chose back into
    // it; the parameter is `*mut NSRect`. No context and no hints are given.
    unsafe { image.CGImageForProposedRect_context_hints(&mut rect, None, None) }
}

/// The image in the file at `path` as pixels, or `None` when there is no readable image there.
pub fn image_file_pixels(
    path: &std::path::Path,
    size: f64,
) -> Option<Retained<objc2_core_graphics::CGImage>> {
    let path = NSString::from_str(path.to_str()?);
    let image =
        objc2_app_kit::NSImage::initWithContentsOfFile(objc2_app_kit::NSImage::alloc(), &path)?;
    image_pixels(&image, size)
}

impl From<&NSRunningApplication> for AppInfo {
    fn from(app: &NSRunningApplication) -> Self {
        AppInfo {
            bundle_id: app.bundle_id().as_deref().map(ToString::to_string),
            localized_name: app.localized_name().as_deref().map(ToString::to_string),
        }
    }
}

impl WindowInfo {
    /// What `element` is, with the window server's record of it: `server_info_hint`, or else, for
    /// a standard window, the record `server` has for its id.
    pub fn from_ax_element(
        element: &AXUIElement,
        server_info_hint: Option<WindowServerInfo>,
        server: &dyn WindowServer,
    ) -> Result<(Self, Option<WindowServerInfo>), AxError> {
        let frame = element.frame()?;
        let role = element.role()?;
        let subrole = element.subrole()?;
        let is_standard = role == AX_WINDOW_ROLE && subrole == AX_STANDARD_WINDOW_SUBROLE;

        let ax_role = Some(role.clone());
        let ax_subrole = Some(subrole.clone());

        let mut server_info = server_info_hint;
        let id = server_info
            .map(|info| info.id)
            .filter(|id| id.as_nonzero().is_some())
            .or_else(|| WindowServerId::try_from(element).ok());
        let is_minimized = element.minimized().unwrap_or_default();
        let is_resizable = element.can_resize().unwrap_or(true);
        let is_modal = element.modal().unwrap_or_default();

        let (bundle_id, path) = if !is_standard {
            (None, None)
        } else if let Some(info) = server_info {
            bundle_info_for_pid(info.pid)
        } else if let Some(window_id) = id {
            server_info = server.get_window(window_id);
            server_info.map(|info| bundle_info_for_pid(info.pid)).unwrap_or((None, None))
        } else {
            (None, None)
        };

        let min_size = server_info.map(|info| info.min_frame).or(None);
        let max_size = server_info.map(|info| info.max_frame).or(None);
        let info = WindowInfo {
            is_standard,
            is_root: true,
            is_minimized,
            is_resizable,
            min_size,
            max_size,
            title: element.title().unwrap_or_default(),
            frame,
            sys_id: id,
            bundle_id,
            path,
            ax_role,
            ax_subrole,
            is_modal,
        };

        Ok((info, server_info))
    }
}

fn bundle_info_for_pid(pid: pid_t) -> (Option<String>, Option<PathBuf>) {
    NSRunningApplication::with_process_id(pid)
        .map(|app| {
            let bundle_id = app.bundle_id().as_deref().map(|b| b.to_string());
            let path = app.bundleURL().as_ref().and_then(|url| {
                let abs_str = url.absoluteString();
                abs_str.as_deref().map(|s| PathBuf::from(s.to_string()))
            });
            (bundle_id, path)
        })
        .unwrap_or((None, None))
}

#[cfg(test)]
mod tests {
    use objc2::rc::Weak;

    use super::*;

    fn watched(app: Retained<NSRunningApplication>) -> Weak<ApplicationObserver> {
        let info = AppInfo {
            bundle_id: None,
            localized_name: None,
        };
        let mut watched = None;
        observe_application(app, info, Arc::new(|_, _| {}), |observer| {
            watched = Some(Weak::new(observer))
        });
        watched.expect("observe is handed the observer")
    }

    /// The map holds the one retain that keeps an observer alive, so removing its entry frees it.
    #[test]
    fn an_application_has_one_observer_until_it_is_removed() {
        let app = NSRunningApplication::currentApplication();
        let pid = app.pid();
        let first = watched(app.clone());
        let again = watched(app);

        let observer = first.load().expect("the map keeps the observer alive");
        assert!(
            again.load().is_some_and(|again| std::ptr::eq(&*again, &*observer)),
            "asked again, the same observer"
        );
        drop(observer);

        remove_application_observer(pid);
        assert!(first.load().is_none(), "removed, it is freed");
    }
}
