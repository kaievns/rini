#![allow(non_upper_case_globals)]
//! Raw declarations for the private SkyLight / CoreGraphics Services API. Declarations only; every
//! caller owns its own safety argument.
// credits
// https://github.com/asmagill/hs._asm.undocumented.spaces/blob/master/CGSSpace.h.
// https://github.com/koekeishiya/yabai/blob/d55a647913ab72d8d8b348bee2d3e59e52ce4a5d/src/misc/extern.h.

use std::ffi::{c_int, c_void};
use std::fmt;
use std::num::NonZeroU32;

use bitflags::bitflags;
use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFString, CFType, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{CGError, CGEventSourceStateID, CGImage, CGWindowID};
use objc2_foundation::NSArray;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

pub static G_CONNECTION: Lazy<cid_t> = Lazy::new(|| unsafe { SLSMainConnectionID() });

#[allow(non_camel_case_types)]
pub type cid_t = i32;

/// Carbon's process identity, still what SkyLight's `SLPS*` calls take.
#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct ProcessSerialNumber {
    pub high: u32,
    pub low: u32,
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[repr(transparent)]
    /// Names the bits returned by `SLSWindowIteratorGetTags`.
    pub struct SLSWindowTags: u64 {
        /// Shows with the standard document-window appearance.
        const DOCUMENT = 1u64 << 0;
        /// Floats above ordinary application windows.
        const FLOATING = 1u64 << 1;
        /// Suppresses Dock badging while the window is minimized.
        const DO_NOT_SHOW_BADGE_IN_DOCK = 1u64 << 2;
        /// Forces the window to render without a shadow.
        const DISABLE_SHADOW = 1u64 << 3;
        /// Requests higher-quality resampling from WindowServer.
        const HIGH_QUALITY_RESAMPLING = 1u64 << 4;
        /// Allows the window to set the cursor while inactive.
        const SETS_CURSOR_IN_BACKGROUND = 1u64 << 5;
        /// Keeps the window responsive during modal run loops.
        const WORKS_WHEN_MODAL = 1u64 << 6;
        /// Anchors the window to another window.
        const ATTACHED = 1u64 << 7;
        /// Ignores the window alpha while dragging.
        const IGNORE_ALPHA_FOR_DRAGGING = 1u64 << 8;
        /// Lets pointer events pass through the window.
        const IGNORE_FOR_EVENTS = 1u64 << 9;
        /// Makes the window intercept pointer events.
        const OPAQUE_FOR_EVENTS = 1u64 << 10;
        /// Shows the window on every workspace.
        const ON_ALL_WORKSPACES = 1u64 << 11;
        /// Bypasses normal CPS pointer-event dispatch.
        const POINTER_EVENTS_AVOID_CPS = 1u64 << 12;
        /// Mirrors AppKit's visible-state tracking.
        const KIT_VISIBLE = 1u64 << 13;
        /// Removes the window from lists when the app deactivates.
        const HIDE_ON_DEACTIVATE = 1u64 << 14;
        /// Prevents ordering the app front when the window appears.
        const AVOIDS_ACTIVATION = 1u64 << 15;
        /// Prevents ordering the app front when the window is selected.
        const PREVENTS_ACTIVATION = 1u64 << 16;
        /// Opts the window out of Option-modifier activation behavior.
        const IGNORES_OPTION = 1u64 << 17;
        /// Excludes the window from standard window cycling.
        const IGNORES_CYCLE = 1u64 << 18;
        /// Defers normal ordering operations for the window.
        const DEFERS_ORDERING = 1u64 << 19;
        /// Defers activation requests for the window.
        const DEFERS_ACTIVATION = 1u64 << 20;
        /// Prevents WindowServer from front-ordering the window.
        const IGNORE_AS_FRONT_WINDOW = 1u64 << 21;
        /// Lets WindowServer handle dragging when the app stalls.
        const ENABLE_SERVER_SIDE_DRAG = 1u64 << 22;
        /// Grabs mouse-down events before normal dispatch.
        const MOUSE_DOWN_EVENTS_GRABBED = 1u64 << 23;
        /// Ignores requests to hide the window.
        const DONT_HIDE = 1u64 << 24;
        /// Prevents the host display from dimming.
        const DONT_DIM_WINDOW_DISPLAY = 1u64 << 25;
        /// Converts all pointers to the window's preferred type.
        const INSTANT_MOUSER_WINDOW = 1u64 << 26;
        /// Follows the user across active-space changes.
        const OWNER_FOLLOWS_FOREGROUND = 1u64 << 27;
        /// Uses distinct active and inactive window levels.
        const ACTIVATION_WINDOW_LEVEL = 1u64 << 28;
        /// Brings the owning app forward when selected.
        const BRING_OWNER_FORWARD = 1u64 << 29;
        /// Allows the window to appear before login completes.
        const PERMITTED_BEFORE_LOGIN = 1u64 << 30;
        /// Marks the window as modal.
        const MODAL = 1u64 << 31;
        /// Marks windows that cooperate with the built-in window manager.
        const WINDOW_MANAGER_AWARE = 1u64 << 32;
        /// Follows the user across the focused document space.
        const FOLLOWS_DOCUMENT_SPACE = 1u64 << 33;
        /// Excludes the window from mirrored-display reflections.
        const NO_MIRROR_REFLECTION = 1u64 << 34;
        /// Enables an internal compositor meshing mode.
        const MESHED = 1u64 << 35;
        /// Marks a window as a current CoreDrag target.
        const CORE_DRAG_IS_DRAGGING = 1u64 << 36;
        /// Excludes the window from screen-capture streams.
        const AVOIDS_CAPTURE = 1u64 << 37;
        /// Excludes the window from Expose processing.
        const IGNORE_FOR_EXPOSE = 1u64 << 38;
        /// Marks the window as hidden.
        const HIDDEN = 1u64 << 39;
        /// Explicitly includes the window in window cycling.
        const INCLUDE_IN_CYCLE = 1u64 << 40;
        /// Captures gestures while the app is inactive.
        const WANTS_GESTURES_IN_BACKGROUND = 1u64 << 41;
        /// Marks the window as fullscreen.
        const FULL_SCREEN = 1u64 << 42;
        /// Marks the window as the accessibility zoom source.
        const MAGIC_ZOOM = 1u64 << 43;
        /// Keeps the window on all spaces through transitions.
        const SUPER_STICKY = 1u64 << 44;
        /// Allows the window to appear over fullscreen apps.
        const FRIEND_OF_FULLSCREEN = 1u64 << 45;
        /// Attaches the window to the menu bar.
        const MENU_BAR = 1u64 << 46;
        /// Gives the window affinity for the desktop level.
        const DESKTOP_AFFINITY = 1u64 << 47;
        /// Forces the window to remain space-bound.
        const NEVER_STICKY = 1u64 << 48;
        /// Places the window at desktop-picture level.
        const DESKTOP_PICTURE = 1u64 << 49;
        /// Disables workspace-placement heuristics for the window.
        const IGNORES_WORKSPACE_HEURISTICS = 1u64 << 50;
        /// Orders the window forward when it flushes.
        const ORDERS_FORWARD_ON_FLUSH = 1u64 << 51;
        /// Marks the window as a user-input accessory.
        const USER_INPUT_ACCESSORY = 1u64 << 52;
        /// Uses a non-standard compositing backing store.
        const NON_COMPOSITING_BACKING_STORE = 1u64 << 53;
        /// Drags the movement-group parent with the window.
        const DRAGS_MOVEMENT_GROUP_PARENT = 1u64 << 54;
        /// Keeps layered surfaces separate during swipe gestures.
        const NEVER_FLATTEN_SURFACES_DURING_SWIPES = 1u64 << 55;
        /// Allows the window to enter native fullscreen mode.
        const FULL_SCREEN_CAPABLE = 1u64 << 56;
        /// Allows the window to join Split View tile spaces.
        const FULL_SCREEN_TILE_CAPABLE = 1u64 << 57;
        /// Excludes the window from screen sharing.
        const IGNORE_FOR_SCREEN_SHARING = 1u64 << 58;
        /// Shares this child window alongside its parent.
        const SHARE_ALONG_WITH_PARENT = 1u64 << 59;
        /// Marks the window as currently miniaturized.
        const MINIATURIZED = 1u64 << 60;
        /// Enables the shared-window indicator state.
        const WINDOW_SHARING_INDICATOR = 1u64 << 61;
        /// Ignores transient ordering changes during filtering.
        const IGNORE_TRANSIENT_ORDERING_FOR_FILTERING = 1u64 << 62;
        /// Marks the window as having a trivial layer tree.
        const TRIVIAL_LAYER_TREE = 1u64 << 63;
    }
}

/// The notify-proc events rini registers or matches on. Every number, with what is known of its
/// payload, is in `docs/README.md`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KnownCGSEvent {
    WindowClosed = 804,
    WindowMoved = 806,
    WindowResized = 807,
    WindowReordered = 808,
    WindowUnhidden = 815,
    WindowHidden = 816,
    SpaceWindowCreated = 1325,
    SpaceWindowDestroyed = 1326,
    SpaceCreated = 1327,
    SpaceDestroyed = 1328,
    WindowManagerSpaceFrontConnectionChanged = 1334,
    WindowManagerGlobalFrontConnectionChanged = 1335,
    SpaceWindowManagementCapabilitiesChanged = 1340,
    ManagedSpaceMembershipUpdated = 1342,
}

impl fmt::Display for KnownCGSEvent {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(self, f) }
}

bitflags! {
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[repr(transparent)]
    pub struct DisplayReconfigFlags: u32 {
        const BEGIN_CONFIGURATION        = 0x0000_0001;
        const MOVED                      = 0x0000_0002;
        const SET_MAIN                   = 0x0000_0004;
        const SET_MODE                   = 0x0000_0008;
        const ADD                        = 0x0000_0010;
        const REMOVE                     = 0x0000_0020;
        const ENABLED                    = 0x0000_0040;
        const DISABLED                   = 0x0000_0080;
        const MIRROR                     = 0x0000_0100;
        const UNMIRROR                   = 0x0000_0200;
        const DESKTOP_SHAPE_CHANGED      = 0x0000_1000;
    }
}

unsafe extern "C" {
    pub fn CFRelease(cf: *mut CFType);

    pub fn _AXUIElementGetWindow(elem: *mut AXUIElement, wid: *mut CGWindowID) -> AXError;

    pub fn CGEventCreate(source: *mut CFType) -> *mut CFType;
    pub fn CGEventSourceCreate(state: CGEventSourceStateID) -> *mut CFType;
    pub fn CGWarpMouseCursorPosition(point: CGPoint) -> CGError;

    /// Physical size of a display in millimetres. Used to map a cursor crossing by real-world
    /// position rather than by fraction, which matters whenever two displays differ in size.
    pub fn CGDisplayScreenSize(display: u32) -> CGSize;
    /// Re-couple the hardware mouse to the on-screen cursor after a warp.
    ///
    /// A warp leaves the cursor carrying whatever velocity it had, so a pointer warped in
    /// at a display edge can slide straight back out. Passing 1 re-associates them.
    pub fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> CGError;
    pub fn CGEventSourceSetLocalEventsSuppressionInterval(source: *mut CFType, interval: f64);

    /// The frame the window server has for a window, answered without asking the app.
    pub fn SLSGetWindowBounds(cid: cid_t, wid: u32, frame: *mut CGRect) -> i32;
    pub fn CGSGetActiveSpace(cid: c_int) -> u64;
    pub fn CGSCopyManagedDisplaySpaces(cid: c_int) -> *mut NSArray;
    pub fn SLSGetSpaceManagementMode(cid: cid_t) -> c_int;
    pub fn CGSManagedDisplayGetCurrentSpace(cid: c_int, uuid: *mut CFString) -> u64;
    pub fn CGSCopyBestManagedDisplayForRect(cid: c_int, rect: CGRect) -> *mut CFString;
    pub fn CGDisplayCreateUUIDFromDisplayID(did: u32) -> *mut CFType;
    pub fn CFUUIDCreateFromString(
        allocator: *mut c_void,
        uuid_string: *mut CFString,
    ) -> *mut CFType;
    pub fn CFUUIDCreateString(allocator: *mut c_void, uuid: *mut CFType) -> *mut CFString;
    pub fn CGDisplayRegisterReconfigurationCallback(
        callback: Option<unsafe extern "C" fn(u32, u32, *mut c_void)>,
        user_info: *mut c_void,
    );

    pub safe fn CGSetLocalEventsSuppressionInterval(int: f32);
    pub safe fn CGEnableEventStateCombining(enable: bool);

    pub fn SLSMainConnectionID() -> cid_t;
    /// A further connection of this process's own, written to `cid`. Pass 0 for `zero`.
    pub fn SLSNewConnection(zero: c_int, cid: *mut cid_t) -> CGError;
    pub fn SLSServerPort(zero: *mut c_void) -> u32;
    pub fn SLSWindowManagementBridgeSetDelegate(delegate: *mut c_void) -> CGError;
    pub fn _SLPSSetFrontProcessWithOptions(
        psn: *const ProcessSerialNumber,
        wid: u32,
        mode: u32,
    ) -> CGError;
    pub fn SLPSGetKeyFocusProcess(psn: *mut ProcessSerialNumber, fallback_flag: *mut u8)
    -> CGError;
    pub fn SLPSPostEventRecordTo(psn: *const ProcessSerialNumber, bytes: *const u8) -> CGError;
    pub fn SLSFindWindowAndOwner(
        cid: c_int,
        zero: c_int,
        one: c_int,
        zero_again: c_int,
        screen_point: *mut CGPoint,
        window_point: *mut CGPoint,
        wid: *mut u32,
        wcid: *mut c_int,
    ) -> i32;
    pub fn SLSGetCurrentCursorLocation(cid: cid_t, point: *mut CGPoint) -> CGError;
    pub fn SLSRegisterConnectionNotifyProc(
        cid: cid_t,
        callback: extern "C" fn(u32, *mut c_void, usize, *mut c_void, cid_t),
        event: u32,
        data: *mut c_void,
    ) -> i32;
    // window_count must be below 1024
    pub fn SLSRequestNotificationsForWindows(
        cid: cid_t,
        window_list: *const u32,
        window_count: i32,
    ) -> i32;
    pub fn SLSManagedDisplaysCopyRoleWindows(
        cid: c_int,
        display_uuids: *mut CFArray<CFString>,
        role: u64,
    ) -> *mut CFArray<CFNumber>;
    pub fn SLSCopyActiveMenuBarDisplayIdentifier(cid: cid_t) -> *mut CFString;
    pub fn SLSSpaceGetType(cid: cid_t, sid: u64) -> c_int;
    pub fn SLSGetMenuBarAutohideEnabled(cid: cid_t, enabled: *mut i32) -> i32;
    pub fn SLSGetDisplayMenubarHeight(did: u32, height: *mut u32) -> i32;
    pub fn CoreDockGetAutoHideEnabled() -> bool;
    pub fn CoreDockGetOrientationAndPinning(orientation: *mut i32, pinning: *mut i32) -> bool;
    pub fn SLSGetDockRectWithReason(cid: cid_t, rect: *mut CGRect, reason: *mut i32) -> bool;
    pub fn CGDisplayIsBuiltin(did: u32) -> bool;
    pub fn CGDisplayGetDisplayIDFromUUID(uuid: *mut CFType) -> u32;

    pub fn SLSWindowQueryWindows(
        cid: c_int,
        windows: *mut CFArray<CFNumber>,
        flags: c_int,
    ) -> *mut CFType;
    pub fn SLSWindowQueryCreate(initial_values: *mut CFDictionary) -> *mut CFType;
    pub fn SLSWindowQuerySetValue(query: *mut CFType, key: *mut CFString, value: *mut CFType);
    pub fn SLSWindowQueryRun(cid: c_int, query: *mut CFType, flags: c_int) -> *mut CFType;
    pub fn SLSWindowQueryResultCopyWindows(query: *mut CFType) -> *mut CFType;
    /// `target_cid`'s windows or, with 0, every connection's, front to back: with `on_screen` 1, those
    /// on screen. Up to `count` written to `list` and how many to `out_count`; `tags_size` is 64.
    pub fn SLSGetWindowListWithTags(
        cid: cid_t,
        target_cid: cid_t,
        count: c_int,
        list: *mut u32,
        out_count: *mut c_int,
        on_screen: c_int,
        set_tags: *mut u64,
        clear_tags: *mut u64,
        tags_size: c_int,
    ) -> CGError;
    /// Every window, on screen or not, of `target_cid`'s or, with 0, of every connection.
    pub fn SLSGetWindowList(
        cid: cid_t,
        target_cid: cid_t,
        count: c_int,
        list: *mut u32,
        out_count: *mut c_int,
    ) -> CGError;

    pub fn SLSWindowIteratorAdvance(iterator: *mut CFType) -> bool;
    pub fn SLSWindowIteratorGetParentID(iterator: *mut CFType) -> u32;
    pub fn SLSWindowIteratorGetWindowID(iterator: *mut CFType) -> u32;
    pub fn SLSWindowIteratorGetTags(iterator: *mut CFType) -> u64;
    pub fn SLSWindowIteratorGetAttributes(iterator: *mut CFType) -> u64;
    pub fn SLSWindowIteratorGetLevel(iterator: *mut CFType) -> c_int;
    pub fn SLSWindowIteratorGetCount(iterator: *mut CFType) -> c_int;
    pub fn SLSWindowIteratorGetPID(iterator: *mut CFType) -> c_int;
    pub fn SLSWindowIteratorGetBounds(iterator: *mut CFType) -> CGRect;
    pub fn SLSWindowIteratorGetAlpha(iterator: *mut CFType) -> f32;
    /// The current window's title, +1, or null; only from a query asked with flag bit 0.
    pub fn SLSWindowIteratorCopyTitle(iterator: *mut CFType) -> *mut CFString;
    pub fn SLSWindowIteratorGetConstraints(
        iterator: *mut CFType,
        min: *mut CGSize,
        max: *mut CGSize,
        cur: *mut CGSize,
    ) -> CGError;
    pub fn SLSPackagesGetWindowConstraints(
        cid: cid_t,
        wid: u32,
        min: *mut CGSize,
        max: *mut CGSize,
        cur: *mut CGSize,
    ) -> CGError;

    pub fn SLSCopySpacesForWindows(
        cid: cid_t,
        selector: u32,
        windows: *mut CFArray<CFNumber>,
    ) -> *mut CFArray<CFNumber>;

    pub fn SLSGetConnectionIDForPSN(
        cid: cid_t,
        psn: *const ProcessSerialNumber,
        out_cid: *mut c_int,
    ) -> c_int;

    pub fn SLSHWCaptureWindowList(
        cid: cid_t,
        window_list: *const u32,
        window_count: c_int,
        options: u32,
    ) -> *mut CFArray<CGImage>;
    /// One picture of the windows, off-screen ones included; with `rect` `CGRectNull`, of their union.
    pub fn SLSCaptureWindowsContentsToRectWithOptions(
        cid: cid_t,
        window_list: *const u32,
        window_count: c_int,
        rect: CGRect,
        options: u32,
        image: *mut *mut CGImage,
    ) -> CGError;

    pub fn SLSSetWindowLevel(cid: cid_t, wid: u32, level: c_int) -> CGError;
    /// Only for a window of the caller's own connection.
    pub fn SLSSetWindowSubLevel(cid: cid_t, wid: u32, sub_level: c_int) -> CGError;
}

/// `SLSWindowIteratorGetCornerRadii`: the current window's four corner radii in points, a CFArray of
/// CFNumbers the caller releases, or null. Not linked: a macOS without it must still launch, so its
/// caller resolves it at run time.
pub type SLSWindowIteratorGetCornerRadiiFn =
    unsafe extern "C" fn(iterator: *mut CFType) -> *mut CFArray<CFNumber>;

/// A CGSSpaceID: the window server's identity for a native space.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct SpaceId(u64);
impl SpaceId {
    pub fn new(id: u64) -> SpaceId { SpaceId(id) }

    pub fn get(&self) -> u64 { self.0 }
}
impl From<SpaceId> for u64 {
    fn from(id: SpaceId) -> u64 { id.get() }
}
impl std::fmt::Display for SpaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

/// A CGWindowID: the window server's identity for a window.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowServerId(pub u32);
impl WindowServerId {
    #[inline]
    pub fn new(id: u32) -> Self { Self(id) }

    #[inline]
    pub fn as_u32(self) -> u32 { self.0 }

    #[inline]
    pub fn as_nonzero(self) -> Option<NonZeroU32> { NonZeroU32::new(self.0) }
}
impl From<WindowServerId> for u32 {
    #[inline]
    fn from(id: WindowServerId) -> Self { id.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_id_serializes_as_a_bare_number() {
        let space = SpaceId::new(1234);
        assert_eq!(serde_json::to_string(&space).unwrap(), "1234");
        assert_eq!(serde_json::from_str::<SpaceId>("1234").unwrap(), space);
        assert_eq!(space.to_string(), "1234");
        assert_eq!(u64::from(space), 1234);
    }

    #[test]
    fn window_server_id_zero_is_not_a_window() {
        assert_eq!(WindowServerId::new(0).as_nonzero(), None);
        assert_eq!(WindowServerId::new(7).as_nonzero().map(|n| n.get()), Some(7));
        assert_eq!(u32::from(WindowServerId::new(7)), 7);
    }
}
