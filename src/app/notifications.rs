//! This actor manages the global notification queue, which tells us when an
//! application is launched or focused or the screen state changes.

use std::cell::Cell;
use std::ffi::c_void;
use std::{future, mem};

use dispatchr::queue;
use dispatchr::time::Time;
use objc2::rc::{Allocated, Retained};
use objc2::{AnyThread, ClassType, DeclaredClass, Encode, Encoding, define_class, msg_send, sel};
use objc2_app_kit::{self, NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey};
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotification, NSNotificationCenter,
    NSNotificationSuspensionBehavior, NSObject, NSProcessInfo, NSString,
};
use rini_runloop::dispatch::DispatchExt;
use rini_skylight_sys::{CGDisplayRegisterReconfigurationCallback, DisplayReconfigFlags};
use tracing::{debug, info_span, trace, warn};

use crate::app::controller::{self, WmEvent};
use crate::app::power::{init_power_state, set_low_power_mode_state};
use crate::displays::platform::spaces;
use crate::windows::platform::app::NSRunningApplicationExt;

#[repr(C)]
struct Instance {
    events_tx: controller::Sender,
    spaces_tx: spaces::Sender,
    session_inactive_hint: Cell<bool>,
}

unsafe impl Encode for Instance {
    const ENCODING: Encoding = Encoding::Object;
}

define_class! {
    // SAFETY:
    // - The superclass NSObject does not have any subclassing requirements.
    // - `NotificationHandler` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[ivars = Box<Instance>]
    struct NotificationCenterInner;

    // SAFETY: Each of these method signatures must match their invocations.
    impl NotificationCenterInner {
        #[unsafe(method_id(initWith:))]
        fn init(this: Allocated<Self>, instance: Instance) -> Option<Retained<Self>> {
            let this = this.set_ivars(Box::new(instance));
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(recvScreenChangedEvent:))]
        fn recv_screen_changed_event(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.handle_screen_changed_event(notif);
        }

        #[unsafe(method(recvAppEvent:))]
        fn recv_app_event(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.handle_app_event(notif);
        }

        #[unsafe(method(recvWakeEvent:))]
        fn recv_wake_event(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.send_space_event(spaces::Notification::SystemDidWake);
        }

        #[unsafe(method(recvSleepEvent:))]
        fn recv_sleep_event(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.send_space_event(spaces::Notification::SystemWillSleep);
        }

        #[unsafe(method(recvSessionEvent:))]
        fn recv_session_event(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.handle_session_event(notif);
        }

        #[unsafe(method(recvPowerEvent:))]
        fn recv_power_event(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.handle_power_event(notif);
        }

        #[unsafe(method(recvMenuBarPrefChanged:))]
        fn recv_menu_bar_pref_changed(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.handle_menu_bar_pref_changed();
        }

        #[unsafe(method(recvDockPrefChanged:))]
        fn recv_dock_pref_changed(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.handle_dock_pref_changed();
        }

        #[unsafe(method(recvKeyboardLayoutChanged:))]
        fn recv_keyboard_layout_changed(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.send_event(WmEvent::KeyboardLayoutChanged);
        }

        #[unsafe(method(recvClockChanged:))]
        fn recv_clock_changed(&self, notif: &NSNotification) {
            trace!("{notif:#?}");
            self.send_event(WmEvent::ClockChanged);
        }
    }
}

/// `NSSystemClockDidChangeNotification` and `NSSystemTimeZoneDidChangeNotification`, by value. The
/// time zone's is Core Foundation's name.
const CLOCK_CHANGED: [&str; 2] = [
    "NSSystemClockDidChangeNotification",
    "kCFTimeZoneSystemTimeZoneDidChangeNotification",
];

/// The notification center a name is observed on. The workspace's is observed for the shared
/// workspace's notifications; the distributed one delivers immediately, even while rini is hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Center {
    Workspace,
    Default,
    Distributed,
}

/// The method of `NotificationCenterInner` a notification is delivered to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Receiver {
    ScreenChanged,
    App,
    Wake,
    Sleep,
    Session,
    DockPref,
    MenuBarPref,
    Power,
    Clock,
    KeyboardLayout,
}

impl Receiver {
    fn selector(self) -> objc2::runtime::Sel {
        match self {
            Receiver::ScreenChanged => sel!(recvScreenChangedEvent:),
            Receiver::App => sel!(recvAppEvent:),
            Receiver::Wake => sel!(recvWakeEvent:),
            Receiver::Sleep => sel!(recvSleepEvent:),
            Receiver::Session => sel!(recvSessionEvent:),
            Receiver::DockPref => sel!(recvDockPrefChanged:),
            Receiver::MenuBarPref => sel!(recvMenuBarPrefChanged:),
            Receiver::Power => sel!(recvPowerEvent:),
            Receiver::Clock => sel!(recvClockChanged:),
            Receiver::KeyboardLayout => sel!(recvKeyboardLayoutChanged:),
        }
    }
}

/// Every notification observed, in the order it is registered: its center, its receiver and its
/// name. Names are by value, as a center matches them; each AppKit constant's is checked in the
/// tests.
const OBSERVED: [(Center, Receiver, &str); 17] = [
    (
        Center::Workspace,
        Receiver::ScreenChanged,
        "NSWorkspaceActiveDisplayDidChangeNotification",
    ),
    (
        Center::Workspace,
        Receiver::ScreenChanged,
        "NSWorkspaceActiveSpaceDidChangeNotification",
    ),
    (
        Center::Default,
        Receiver::ScreenChanged,
        "NSApplicationDidChangeScreenParametersNotification",
    ),
    (
        Center::Workspace,
        Receiver::Wake,
        "NSWorkspaceDidWakeNotification",
    ),
    (
        Center::Workspace,
        Receiver::Sleep,
        "NSWorkspaceWillSleepNotification",
    ),
    (
        Center::Workspace,
        Receiver::App,
        "NSWorkspaceDidActivateApplicationNotification",
    ),
    (
        Center::Workspace,
        Receiver::App,
        "NSWorkspaceDidDeactivateApplicationNotification",
    ),
    (
        Center::Workspace,
        Receiver::Session,
        "NSWorkspaceSessionDidResignActiveNotification",
    ),
    (
        Center::Workspace,
        Receiver::Session,
        "NSWorkspaceSessionDidBecomeActiveNotification",
    ),
    (Center::Default, Receiver::DockPref, "com.apple.dock.prefchanged"),
    (
        Center::Default,
        Receiver::MenuBarPref,
        "AppleInterfaceMenuBarHidingChangedNotification",
    ),
    (
        Center::Default,
        Receiver::Power,
        "NSProcessInfoPowerStateDidChangeNotification",
    ),
    (Center::Default, Receiver::Clock, CLOCK_CHANGED[0]),
    (Center::Default, Receiver::Clock, CLOCK_CHANGED[1]),
    (
        Center::Distributed,
        Receiver::KeyboardLayout,
        "com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged",
    ),
    (
        Center::Distributed,
        Receiver::Session,
        "com.apple.screenIsLocked",
    ),
    (
        Center::Distributed,
        Receiver::Session,
        "com.apple.screenIsUnlocked",
    ),
];

impl NotificationCenterInner {
    fn new(events_tx: controller::Sender, spaces_tx: spaces::Sender) -> Retained<Self> {
        let instance = Instance {
            events_tx,
            spaces_tx,
            session_inactive_hint: Cell::new(false),
        };
        let handler: Retained<Self> = unsafe { msg_send![Self::alloc(), initWith: instance] };
        unsafe {
            CGDisplayRegisterReconfigurationCallback(
                Some(Self::display_reconfig_callback),
                Retained::<NotificationCenterInner>::as_ptr(&handler) as *mut c_void,
            );
        }
        handler
    }

    fn enter_session_inactive(&self) {
        if !self.ivars().session_inactive_hint.replace(true) {
            self.send_space_event(spaces::Notification::SessionDidResignActive);
        }
    }

    fn leave_session_inactive(&self) {
        if self.ivars().session_inactive_hint.replace(false) {
            self.send_space_event(spaces::Notification::SessionDidBecomeActive);
        }
    }

    fn handle_screen_changed_event(&self, notif: &NSNotification) {
        use objc2_app_kit::*;
        let name = &*notif.name();
        let span = info_span!("notification_center::handle_screen_changed_event", ?name);
        let _s = span.enter();
        if name.to_string() == "NSWorkspaceActiveDisplayDidChangeNotification" {
            self.send_space_event(spaces::Notification::ActiveDisplayChanged);
        } else if unsafe { NSWorkspaceActiveSpaceDidChangeNotification } == name {
            self.send_space_event(spaces::Notification::ActiveSpaceChanged);
        } else if unsafe { NSApplicationDidChangeScreenParametersNotification } == name {
            self.send_space_event(spaces::Notification::ScreenRefreshRequested);
        } else {
            warn!("Unexpected screen changed event: {notif:?}");
        }
    }

    fn handle_session_event(&self, notif: &NSNotification) {
        use objc2_app_kit::*;
        let name = &*notif.name();
        let span = info_span!("notification_center::handle_session_event", ?name);
        let _guard = span.enter();

        if unsafe { NSWorkspaceSessionDidResignActiveNotification } == name
            || name.to_string() == "com.apple.screenIsLocked"
        {
            self.enter_session_inactive();
        } else if unsafe { NSWorkspaceSessionDidBecomeActiveNotification } == name
            || name.to_string() == "com.apple.screenIsUnlocked"
        {
            self.leave_session_inactive();
        } else {
            warn!("Unexpected session event: {notif:?}");
        }
    }

    fn handle_power_event(&self, _notif: &NSNotification) {
        let span = info_span!("notification_center::handle_power_event");
        let _s = span.enter();

        let process_info = NSProcessInfo::processInfo();
        let current_state = process_info.isLowPowerModeEnabled();
        let old_state = set_low_power_mode_state(current_state);

        if old_state != current_state {
            debug!("Low power mode changed: {} -> {}", old_state, current_state);
            self.send_event(WmEvent::PowerStateChanged(current_state));
        }
    }

    fn handle_app_event(&self, notif: &NSNotification) {
        use objc2_app_kit::*;
        let Some(app) = self.running_application(notif) else {
            return;
        };
        let pid = app.pid();
        let name = &*notif.name();
        let span = info_span!("notification_center::handle_app_event", ?name);
        let _guard = span.enter();
        if unsafe { NSWorkspaceDidDeactivateApplicationNotification } == name {
            self.send_event(WmEvent::AppGloballyDeactivated(pid));
        } else if unsafe { NSWorkspaceDidActivateApplicationNotification } == name {
            // Do not forward AppGloballyActivated from NSWorkspace here.
            //
            // Rini intentionally treats workspace app-activation notifications as a
            // lock/login hint channel only. The authoritative global activation
            // stream comes from the Carbon process actor (`K_EVENT_APP_FRONT_SWITCHED`),
            // which still drives the reactor's activation-time visible-window refresh
            // and workspace-switch behavior. Re-emitting activation here would create
            // duplicate front-app events; the only notification-center-specific job is
            // to notice loginwindow transitions that Carbon does not model as a
            // session-lock boundary.
            let bundle_id = app.bundle_id().as_deref().map(ToString::to_string);
            if bundle_id.as_deref() == Some("com.apple.loginwindow") {
                // OmniWM found loginwindow activation to be a more reliable lock
                // boundary than the distributed lock notification alone. Route it
                // through the same session event stream so the spaces actor
                // buffers topology while macOS swaps in the lock-screen spaces.
                self.enter_session_inactive();
            } else if self.should_leave_session_inactive_for_activation(pid, bundle_id.as_deref()) {
                self.leave_session_inactive();
            }
        }
    }

    fn should_leave_session_inactive_for_activation(
        &self,
        activated_pid: i32,
        activated_bundle_id: Option<&str>,
    ) -> bool {
        let frontmost = NSWorkspace::sharedWorkspace().frontmostApplication();
        let frontmost_pid = frontmost.as_ref().map(|app| app.pid());
        let frontmost_bundle_id = frontmost
            .as_ref()
            .and_then(|app| app.bundle_id().as_deref().map(ToString::to_string));

        should_leave_session_inactive_after_non_login_activation(
            self.ivars().session_inactive_hint.get(),
            activated_pid,
            activated_bundle_id,
            frontmost_pid,
            frontmost_bundle_id.as_deref(),
        )
    }

    fn send_event(&self, event: WmEvent) { self.ivars().events_tx.send(event); }

    fn send_space_event(&self, event: spaces::Notification) { self.ivars().spaces_tx.send(event); }

    fn running_application(
        &self,
        notif: &NSNotification,
    ) -> Option<Retained<NSRunningApplication>> {
        let info = notif.userInfo();
        let Some(info) = info else {
            warn!("Got app notification without user info: {notif:?}");
            return None;
        };
        let app = unsafe { info.valueForKey(NSWorkspaceApplicationKey) };
        let Some(app) = app else {
            warn!("Got app notification without app object: {notif:?}");
            return None;
        };
        assert!(app.class() == NSRunningApplication::class());
        let app: Retained<NSRunningApplication> = unsafe { mem::transmute(app) };
        Some(app)
    }

    fn handle_dock_pref_changed(&self) {
        trace!("Dock preferences changed; scheduling refresh");
        self.send_space_event(spaces::Notification::ScreenRefreshRequested);
    }

    fn handle_menu_bar_pref_changed(&self) {
        trace!("Menu bar autohide changed; scheduling refresh");
        self.send_space_event(spaces::Notification::ScreenRefreshRequested);
    }

    unsafe extern "C" fn display_reconfig_callback(
        _display_id: u32,
        flags: u32,
        user_info: *mut c_void,
    ) {
        if user_info.is_null() {
            return;
        }
        /// The handler, dereferenced only by the work below, on the main queue.
        struct OnMain(*const NotificationCenterInner);
        // SAFETY: the pointer crosses to the main queue and is dereferenced only there, on the main
        // thread that made the handler and is the only one to use it.
        unsafe impl Send for OnMain {}
        let handler = OnMain(user_info.cast::<NotificationCenterInner>().cast_const());
        let parsed = DisplayReconfigFlags::from_bits_truncate(flags);
        queue::main().after_f_s(Time::NOW, (handler, parsed), |(handler, flags)| {
            // SAFETY: `user_info` is the handler `new` registered, which `watch_for_notifications`,
            // never returning, keeps alive.
            let handler = unsafe { &*handler.0 };
            handler.send_space_event(spaces::Notification::DisplayReconfigured { flags });
        });
    }
}

fn should_leave_session_inactive_after_non_login_activation(
    session_inactive_hint: bool,
    activated_pid: i32,
    activated_bundle_id: Option<&str>,
    frontmost_pid: Option<i32>,
    frontmost_bundle_id: Option<&str>,
) -> bool {
    session_inactive_hint
        && activated_bundle_id != Some("com.apple.loginwindow")
        && frontmost_bundle_id != Some("com.apple.loginwindow")
        && frontmost_pid == Some(activated_pid)
}

pub struct NotificationCenter {
    inner: Retained<NotificationCenterInner>,
}

impl NotificationCenter {
    pub fn new(events_tx: controller::Sender, spaces_tx: spaces::Sender) -> Self {
        let handler = NotificationCenterInner::new(events_tx.clone(), spaces_tx);

        let workspace = &NSWorkspace::sharedWorkspace();
        let workspace_center = &workspace.notificationCenter();
        let default_center = &NSNotificationCenter::defaultCenter();
        let distributed_center = &NSDistributedNotificationCenter::defaultCenter();
        for &(center, receiver, name) in &OBSERVED {
            let name = NSString::from_str(name);
            let selector = receiver.selector();
            // SAFETY: each receiver's selector is a method of `NotificationCenterInner` with the
            // signature fn(&self, &NSNotification).
            unsafe {
                match center {
                    Center::Workspace => workspace_center.addObserver_selector_name_object(
                        &handler,
                        selector,
                        Some(&name),
                        Some(workspace),
                    ),
                    Center::Default => default_center.addObserver_selector_name_object(
                        &handler,
                        selector,
                        Some(&name),
                        None,
                    ),
                    Center::Distributed => distributed_center
                        .addObserver_selector_name_object_suspensionBehavior(
                            &handler,
                            selector,
                            Some(&name),
                            None,
                            NSNotificationSuspensionBehavior::DeliverImmediately,
                        ),
                }
            }
        }

        init_power_state();

        NotificationCenter { inner: handler }
    }

    pub async fn watch_for_notifications(self) {
        let workspace = &NSWorkspace::sharedWorkspace();

        self.inner.send_space_event(spaces::Notification::ScreenRefreshRequested);
        self.inner.send_event(WmEvent::AppEventsRegistered);
        if let Some(app) = workspace.frontmostApplication() {
            if app.bundle_id().as_deref().map(ToString::to_string).as_deref()
                == Some("com.apple.loginwindow")
            {
                self.inner.enter_session_inactive();
            }
            self.inner.send_event(WmEvent::AppGloballyActivated(app.pid()));
        }

        future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use objc2_foundation::NSString;

    use super::{
        CLOCK_CHANGED, Center, OBSERVED, Receiver,
        should_leave_session_inactive_after_non_login_activation,
    };

    unsafe extern "C" {
        static NSSystemClockDidChangeNotification: &'static NSString;
        static NSSystemTimeZoneDidChangeNotification: &'static NSString;
    }

    /// The names observed are Foundation's own constants. The time zone's is not its symbol's name.
    #[test]
    fn the_clock_notifications_are_foundations_own() {
        // SAFETY: both are immutable `NSString` constants exported by Foundation.
        let foundation = unsafe {
            [
                NSSystemClockDidChangeNotification,
                NSSystemTimeZoneDidChangeNotification,
            ]
        };
        assert_eq!(
            foundation.map(|name| name.to_string()),
            CLOCK_CHANGED.map(String::from)
        );
    }

    /// `OBSERVED` is every notification on its center, delivered to its receiver, in registration
    /// order, with each AppKit name its constant's value, so observing it by value observes the
    /// constant.
    #[test]
    fn every_notification_is_observed_on_its_center_by_its_receiver() {
        use Center::{Default, Distributed, Workspace};
        use objc2_app_kit::*;
        let appkit = |name: &NSString| name.to_string();
        // SAFETY: each is an immutable `NSString` constant exported by AppKit.
        let expected: Vec<(Center, Receiver, String)> = unsafe {
            vec![
                (
                    Workspace,
                    Receiver::ScreenChanged,
                    "NSWorkspaceActiveDisplayDidChangeNotification".into(),
                ),
                (
                    Workspace,
                    Receiver::ScreenChanged,
                    appkit(NSWorkspaceActiveSpaceDidChangeNotification),
                ),
                (
                    Default,
                    Receiver::ScreenChanged,
                    appkit(NSApplicationDidChangeScreenParametersNotification),
                ),
                (Workspace, Receiver::Wake, appkit(NSWorkspaceDidWakeNotification)),
                (
                    Workspace,
                    Receiver::Sleep,
                    appkit(NSWorkspaceWillSleepNotification),
                ),
                (
                    Workspace,
                    Receiver::App,
                    appkit(NSWorkspaceDidActivateApplicationNotification),
                ),
                (
                    Workspace,
                    Receiver::App,
                    appkit(NSWorkspaceDidDeactivateApplicationNotification),
                ),
                (
                    Workspace,
                    Receiver::Session,
                    appkit(NSWorkspaceSessionDidResignActiveNotification),
                ),
                (
                    Workspace,
                    Receiver::Session,
                    appkit(NSWorkspaceSessionDidBecomeActiveNotification),
                ),
                (Default, Receiver::DockPref, "com.apple.dock.prefchanged".into()),
                (
                    Default,
                    Receiver::MenuBarPref,
                    "AppleInterfaceMenuBarHidingChangedNotification".into(),
                ),
                (
                    Default,
                    Receiver::Power,
                    "NSProcessInfoPowerStateDidChangeNotification".into(),
                ),
                (Default, Receiver::Clock, CLOCK_CHANGED[0].into()),
                (Default, Receiver::Clock, CLOCK_CHANGED[1].into()),
                (
                    Distributed,
                    Receiver::KeyboardLayout,
                    "com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged".into(),
                ),
                (Distributed, Receiver::Session, "com.apple.screenIsLocked".into()),
                (
                    Distributed,
                    Receiver::Session,
                    "com.apple.screenIsUnlocked".into(),
                ),
            ]
        };
        let observed: Vec<(Center, Receiver, String)> = OBSERVED
            .iter()
            .map(|&(center, receiver, name)| (center, receiver, name.to_string()))
            .collect();
        assert_eq!(observed, expected);
    }

    /// Each name is observed once on its center, and the session is followed both on the
    /// workspace and through the distributed lock and unlock.
    #[test]
    fn each_name_is_observed_once_and_the_session_on_both_centers() {
        for (index, &(center, _, name)) in OBSERVED.iter().enumerate() {
            assert!(
                !OBSERVED[..index].iter().any(|&(c, _, n)| c == center && n == name),
                "{name} twice"
            );
        }
        let session: Vec<(Center, &str)> = OBSERVED
            .iter()
            .filter(|(_, receiver, _)| *receiver == Receiver::Session)
            .map(|&(center, _, name)| (center, name))
            .collect();
        assert_eq!(session, [
            (
                Center::Workspace,
                "NSWorkspaceSessionDidResignActiveNotification"
            ),
            (
                Center::Workspace,
                "NSWorkspaceSessionDidBecomeActiveNotification"
            ),
            (Center::Distributed, "com.apple.screenIsLocked"),
            (Center::Distributed, "com.apple.screenIsUnlocked"),
        ]);
    }

    #[test]
    fn unlock_fallback_requires_session_to_be_marked_inactive() {
        assert!(!should_leave_session_inactive_after_non_login_activation(
            false,
            42,
            Some("com.example.app"),
            Some(42),
            Some("com.example.app"),
        ));
    }

    #[test]
    fn unlock_fallback_rejects_loginwindow_or_non_frontmost_activations() {
        assert!(!should_leave_session_inactive_after_non_login_activation(
            true,
            42,
            Some("com.example.app"),
            Some(7),
            Some("com.example.app"),
        ));
        assert!(!should_leave_session_inactive_after_non_login_activation(
            true,
            42,
            Some("com.example.app"),
            Some(42),
            Some("com.apple.loginwindow"),
        ));
        assert!(!should_leave_session_inactive_after_non_login_activation(
            true,
            42,
            Some("com.apple.loginwindow"),
            Some(42),
            Some("com.apple.loginwindow"),
        ));
    }

    #[test]
    fn unlock_fallback_accepts_matching_frontmost_non_login_activation() {
        assert!(should_leave_session_inactive_after_non_login_activation(
            true,
            42,
            Some("com.example.app"),
            Some(42),
            Some("com.example.app"),
        ));
    }
}
