//! Window-server reads the application still makes directly: the Dock's Mission Control overlay,
//! the desktop window of a display, a window's sub-level. The contexts' own reads live with them.
use std::ffi::c_int;

use objc2_core_foundation::{CFDictionary, CFString, CFType};
use objc2_core_graphics::{kCGWindowLayer, kCGWindowName, kCGWindowOwnerName};
use rini_displays::screen::ScreenInfo;
use rini_windows::window_server::{get_num, get_visible_windows_raw};

use crate::mach::mach_get_window_sub_level;

fn get_string(dict: &CFDictionary<CFString, CFType>, key: &'static CFString) -> Option<String> {
    Some(dict.get(key)?.downcast::<CFString>().ok()?.to_string())
}
/// Global CG on-screen window snapshot.
///
/// This is intentionally *not* space-aware and should not be used for ordinary
/// reactor/spaces reconciliation. Native-space truth comes from the spaces actor
/// via `space_window_list_for_connection(...)`.
/// Returns whether Mission Control's Dock-owned layer-18 overlay is still
/// visible in the global on-screen window list.
///
/// This intentionally uses the global CG on-screen snapshot because the signal
/// we want is the presence of the Dock's Mission Control UI itself, not
/// ordinary native-space membership.
pub fn mission_control_dock_overlay_visible() -> bool {

    const MISSION_CONTROL_DOCK_LAYER: i64 = 18;

    get_visible_windows_raw::<CFDictionary<CFString, CFType>>()
        .iter()
        .any(|window| {
            if window.get(unsafe { kCGWindowName }).is_some() {
                return false;
            }

            let Some(owner_name) = get_string(&window, unsafe { kCGWindowOwnerName }) else {
                return false;
            };
            if owner_name != "Dock" {
                return false;
            }

            get_num(&window, unsafe { kCGWindowLayer }) == Some(MISSION_CONTROL_DOCK_LAYER)
        })
}
#[cfg(not(any(test, feature = "test-support")))]
pub fn focus_desktop_window(screen: &ScreenInfo) -> bool {
    use objc2_core_foundation::{CFArray, CFRetained};
    use rini_shared::geometry::CGRectExt;
    use rini_skylight_sys::{G_CONNECTION, SLSManagedDisplaysCopyRoleWindows};
    use rini_windows::ids::WindowServerId;
    use rini_windows::window_server::{get_window, make_key_window};
    use std::ptr::NonNull;
    let Some(display_uuid) = screen.display_uuid_opt() else {
        return false;
    };
    let uuid = CFString::from_str(display_uuid);
    let displays = CFArray::from_objects(&[&*uuid]);
    let Some(windows) = NonNull::new(unsafe {
        SLSManagedDisplaysCopyRoleWindows(*G_CONNECTION, CFRetained::as_ptr(&displays).as_ptr(), 1)
    }) else {
        return false;
    };
    let windows = unsafe { CFRetained::from_raw(windows) };
    windows.iter().any(|number| {
        let Some(id) = number.as_i64().and_then(|id| u32::try_from(id).ok()) else {
            return false;
        };
        let wsid = WindowServerId::new(id);
        let Some(info) = get_window(wsid) else {
            return false;
        };
        info.layer < 0
            && screen.frame.contains(info.frame.mid())
            && make_key_window(info.pid, wsid).is_ok()
    })
}
#[cfg(any(test, feature = "test-support"))]
pub fn focus_desktop_window(_screen: &ScreenInfo) -> bool {
    false
}
pub fn window_sub_level(wid: u32) -> c_int {
    unsafe { mach_get_window_sub_level(wid) }
}
