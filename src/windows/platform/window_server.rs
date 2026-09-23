#[cfg(test)]
use std::cell::RefCell;
use std::ffi::{CStr, c_int};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use libc::{RTLD_DEFAULT, dlsym};
use objc2_app_kit::{NSNormalWindowLevel, NSWindowLevel};
use objc2_application_services::AXError;
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize, Type,
};
use objc2_core_graphics::{
    CGError, CGWindowID, CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID,
    kCGWindowBounds, kCGWindowLayer, kCGWindowNumber,
};
use once_cell::sync::Lazy;
use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};
use rini_geometry::CGRectExt;
use rini_skylight_sys::*;
#[cfg(test)]
use rustc_hash::FxHashMap as HashMap;

use crate::windows::domain::info::WindowServerInfo;
use crate::windows::platform::ax::element::{AXUIElement, Error as AxError};
use crate::windows::platform::cg_ok;
#[cfg(not(test))]
use crate::windows::platform::process::ProcessSerialNumber;
use crate::windows::platform::sub_level::window_sub_level;

static G_CONNECTION: Lazy<i32> = Lazy::new(|| unsafe { SLSMainConnectionID() });
static LAST_WINDOWSERVER_ACTIVITY_US: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
thread_local! {
    static TEST_SPACE_WINDOW_LIST_OVERRIDE: RefCell<Option<Vec<u32>>> = const { RefCell::new(None) };
    static TEST_SPACE_WINDOW_LIST_BY_SPACE_OVERRIDE: RefCell<HashMap<u64, Vec<u32>>> = RefCell::new(HashMap::default());
    static TEST_WINDOW_SPACES_OVERRIDE: RefCell<HashMap<u32, Vec<u64>>> = RefCell::new(HashMap::default());
    static TEST_WINDOW_ORDERED_IN_OVERRIDE: RefCell<HashMap<u32, bool>> = RefCell::new(HashMap::default());
    static TEST_FRONT_TO_BACK_OVERRIDE: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

pub const WINDOWSERVER_QUIET_US: u64 = 350_000;
#[cfg_attr(test, allow(dead_code))]
const EFFECTIVELY_INVISIBLE_WINDOW_ALPHA: f32 = 0.01;

impl TryFrom<&AXUIElement> for WindowServerId {
    type Error = AxError;

    fn try_from(element: &AXUIElement) -> Result<Self, Self::Error> {
        let mut id = 0;
        let res = unsafe { _AXUIElementGetWindow(element.raw_ptr().as_ptr(), &mut id) };
        if res != AXError::Success {
            return Err(AxError::Ax(res));
        }
        if id == 0 {
            return Err(AxError::NotFound);
        }
        Ok(Self(id))
    }
}

#[inline]
fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64
}

pub fn note_windowserver_activity(wsid: u32) {
    LAST_WINDOWSERVER_ACTIVITY_US.store(now_us(), Ordering::SeqCst);
    // Keep this trace low-cost; it's only used to stabilize display churn.
    tracing::trace!(wsid, "windowserver activity");
}

pub fn windowserver_quiet_for_us(quiet_us: u64) -> bool {
    let last = LAST_WINDOWSERVER_ACTIVITY_US.load(Ordering::SeqCst);
    if last == 0 {
        return true;
    }
    now_us().saturating_sub(last) >= quiet_us
}

#[inline]
fn cf_array_from_ids(ids: &[WindowServerId]) -> CFRetained<CFArray<CFNumber>> {
    if let [id] = ids {
        let number = CFNumber::new_i64(id.as_u32() as i64);
        return CFArray::from_retained_objects(std::slice::from_ref(&number));
    }
    let nums: Vec<CFRetained<CFNumber>> =
        ids.iter().map(|w| CFNumber::new_i64(w.as_u32() as i64)).collect();
    CFArray::from_retained_objects(&nums)
}

#[inline]
fn cf_array_from_u64s(ids: &[u64]) -> CFRetained<CFArray<CFNumber>> {
    if let [id] = ids {
        let number = CFNumber::new_i64(*id as i64);
        return CFArray::from_retained_objects(std::slice::from_ref(&number));
    }
    let nums: Vec<CFRetained<CFNumber>> =
        ids.iter().map(|&id| CFNumber::new_i64(id as i64)).collect();
    CFArray::from_retained_objects(&nums)
}

pub struct WindowIterator {
    iter: *mut CFType,
}

impl WindowIterator {
    pub fn new(ids: &[WindowServerId]) -> Option<Self> {
        if ids.is_empty() {
            return None;
        }
        let cf_numbers = cf_array_from_ids(ids);
        Self::new_from_cfarray(CFRetained::as_ptr(&cf_numbers).as_ptr(), 0)
    }

    /// `flags` controls optional result decoding; bit 0 requests window titles.
    fn new_from_cfarray(cf_numbers: *mut CFArray<CFNumber>, flags: c_int) -> Option<Self> {
        let query = unsafe { SLSWindowQueryWindows(*G_CONNECTION, cf_numbers, flags) };
        if query.is_null() {
            return None;
        }
        let iter = unsafe { SLSWindowQueryResultCopyWindows(query) };
        unsafe { CFRelease(query) };
        if iter.is_null() {
            return None;
        }
        Self::from_owned_iterator(iter)
    }

    fn from_owned_iterator(iter: *mut CFType) -> Option<Self> {
        if iter.is_null() {
            return None;
        }
        let iterator = Self { iter };
        // Settle the SLS iterator before its first Advance. Some reply shapes
        // otherwise produce a valid iterator that initially appears empty.
        let _ = iterator.count();
        Some(iterator)
    }

    #[inline]
    pub fn count(&self) -> i32 { unsafe { SLSWindowIteratorGetCount(self.iter) } }

    #[inline]
    pub fn advance<'a>(&'a self) -> Option<&'a Self> {
        if unsafe { SLSWindowIteratorAdvance(self.iter) } {
            return Some(self);
        }

        None
    }

    #[inline]
    pub fn window_id(&self) -> u32 { unsafe { SLSWindowIteratorGetWindowID(self.iter) } }

    #[inline]
    pub fn level(&self) -> i32 { unsafe { SLSWindowIteratorGetLevel(self.iter) } }

    #[inline]
    pub fn pid(&self) -> i32 { unsafe { SLSWindowIteratorGetPID(self.iter) } }

    #[inline]
    pub fn parent_id(&self) -> u32 { unsafe { SLSWindowIteratorGetParentID(self.iter) } }

    #[inline]
    pub fn bounds(&self) -> CGRect { unsafe { SLSWindowIteratorGetBounds(self.iter) } }

    #[inline]
    pub fn alpha(&self) -> f32 { unsafe { SLSWindowIteratorGetAlpha(self.iter) } }

    #[inline]
    #[allow(dead_code)]
    pub fn tags(&self) -> u64 { unsafe { SLSWindowIteratorGetTags(self.iter) } }

    #[inline]
    #[allow(dead_code)]
    pub fn attributes(&self) -> u64 { unsafe { SLSWindowIteratorGetAttributes(self.iter) } }

    #[inline]
    pub fn constraints(&self) -> (CGSize, CGSize) {
        let mut min = CGSize::ZERO;
        let mut max = CGSize::ZERO;
        let mut cur = CGSize::ZERO;
        unsafe { SLSWindowIteratorGetConstraints(self.iter, &mut min, &mut max, &mut cur) };

        if min.width == 0.0 && min.height == 0.0 && max.width == 0.0 && max.height == 0.0 {
            unsafe {
                SLSPackagesGetWindowConstraints(
                    *G_CONNECTION,
                    self.window_id(),
                    &mut min,
                    &mut max,
                    &mut cur,
                )
            };
        }

        (min, max)
    }
}

impl Drop for WindowIterator {
    fn drop(&mut self) { unsafe { CFRelease(self.iter) } }
}

/// Server-side filter for `SLSWindowQueryRun`.
///
/// Unlike [`WindowIterator`], this describes which windows WindowServer should
/// materialize. Results are returned in native z-order.
#[derive(Debug, Clone, Copy)]
struct WindowQueryFilter<'a> {
    owner: i32,
    spaces: &'a [u64],
    space_list_options: i32,
    window_list_options: i32,
    query_flags: i32,
    include_tags: u64,
    exclude_tags: u64,
}

#[derive(Clone, Copy)]
struct WindowQueryKeys {
    owner: usize,
    spaces: usize,
    space_options: usize,
    window_options: usize,
    include_tags: usize,
    exclude_tags: usize,
}

fn resolve_window_query_key(name: &CStr) -> Option<usize> {
    let slot = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
    if slot.is_null() {
        return None;
    }
    let key = unsafe { *(slot.cast::<*mut CFString>()) };
    (!key.is_null()).then_some(key as usize)
}

static WINDOW_QUERY_KEYS: Lazy<Option<WindowQueryKeys>> = Lazy::new(|| {
    Some(WindowQueryKeys {
        owner: resolve_window_query_key(c"SLSWindowQueryKeyOwner")?,
        spaces: resolve_window_query_key(c"SLSWindowQueryKeySpaces").unwrap_or(0),
        space_options: resolve_window_query_key(c"SLSWindowQueryKeySpaceListOptions").unwrap_or(0),
        // This key appeared later than the core query API. A missing value is
        // represented by zero and simply leaves the server default in place.
        window_options: resolve_window_query_key(c"SLSWindowQueryKeyWorkspaceWindowListOptions")
            .unwrap_or(0),
        include_tags: resolve_window_query_key(c"SLSWindowQueryKeyIncludeTags")?,
        exclude_tags: resolve_window_query_key(c"SLSWindowQueryKeyExcludeTags")?,
    })
});

unsafe fn set_window_query_value<T: Type>(query: *mut CFType, key: usize, value: &CFRetained<T>) {
    if key != 0 {
        unsafe {
            SLSWindowQuerySetValue(
                query,
                key as *mut CFString,
                CFRetained::as_ptr(value).as_ptr().cast::<CFType>(),
            )
        };
    }
}

/// Run a native, server-side window query and return its z-ordered iterator.
fn window_query_run(filter: &WindowQueryFilter<'_>) -> Option<WindowIterator> {
    let keys = (*WINDOW_QUERY_KEYS)?;
    let uses_explicit_spaces = !filter.spaces.is_empty();
    if (uses_explicit_spaces && keys.spaces == 0)
        || (!uses_explicit_spaces && keys.space_options == 0)
    {
        return None;
    }

    let query = unsafe { SLSWindowQueryCreate(std::ptr::null_mut()) };
    if query.is_null() {
        return None;
    }

    let owner = CFNumber::new_i32(filter.owner);
    let include_tags = CFNumber::new_i64(filter.include_tags as i64);
    let exclude_tags = CFNumber::new_i64(filter.exclude_tags as i64);
    let window_options =
        (keys.window_options != 0).then(|| CFNumber::new_i32(filter.window_list_options));
    unsafe {
        set_window_query_value(query, keys.owner, &owner);
        set_window_query_value(query, keys.include_tags, &include_tags);
        set_window_query_value(query, keys.exclude_tags, &exclude_tags);
        if let Some(window_options) = &window_options {
            set_window_query_value(query, keys.window_options, window_options);
        }
    }

    let space_array = uses_explicit_spaces.then(|| cf_array_from_u64s(filter.spaces));
    let space_options =
        (!uses_explicit_spaces).then(|| CFNumber::new_i32(filter.space_list_options));
    unsafe {
        if let Some(space_array) = &space_array {
            set_window_query_value(query, keys.spaces, space_array);
        } else if let Some(space_options) = &space_options {
            set_window_query_value(query, keys.space_options, space_options);
        }
    }

    let result = unsafe { SLSWindowQueryRun(*G_CONNECTION, query, filter.query_flags) };
    let iterator = if result.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { SLSWindowQueryResultCopyWindows(result) }
    };
    unsafe {
        if !result.is_null() {
            CFRelease(result);
        }
        CFRelease(query);
    }
    WindowIterator::from_owned_iterator(iterator)
}

pub fn window_parent(id: WindowServerId) -> Option<WindowServerId> {
    let query = WindowIterator::new(&[id])?;
    if query.count() == 1 {
        let p = query.advance()?.parent_id();
        (p != 0).then(|| WindowServerId::new(p))
    } else {
        None
    }
}

pub fn window_is_sticky(id: WindowServerId) -> bool {
    let cf_windows = cf_array_from_ids(&[id]);
    let space_list_ref = unsafe {
        SLSCopySpacesForWindows(*G_CONNECTION, 0x7, CFRetained::as_ptr(&cf_windows).as_ptr())
    };
    let Some(space_list_ref) = NonNull::new(space_list_ref) else {
        return false;
    };
    let spaces_cf: CFRetained<CFArray<CFNumber>> = unsafe { CFRetained::from_raw(space_list_ref) };
    spaces_cf.len() > 1
}

/// The spaces the window server places this window on.
///
/// A test with no override gets nothing: window server ids collide with real ones. See "A unit test must
/// not read the live window server" in `docs/testing.md`.
#[cfg(test)]
pub fn window_spaces(id: WindowServerId) -> Vec<SpaceId> {
    TEST_WINDOW_SPACES_OVERRIDE
        .with(|spaces| spaces.borrow().get(&id.as_u32()).cloned())
        .unwrap_or_default()
        .into_iter()
        .map(SpaceId::new)
        .collect()
}

#[cfg(not(test))]
pub fn window_spaces(id: WindowServerId) -> Vec<SpaceId> {
    let cf_windows = cf_array_from_ids(&[id]);
    let space_list_ref = unsafe {
        SLSCopySpacesForWindows(*G_CONNECTION, 0x7, CFRetained::as_ptr(&cf_windows).as_ptr())
    };
    let Some(space_list_ref) = NonNull::new(space_list_ref) else {
        return Vec::new();
    };

    let spaces_cf: CFRetained<CFArray<CFNumber>> = unsafe { CFRetained::from_raw(space_list_ref) };
    spaces_cf
        .iter()
        .filter_map(|num| num.as_i64())
        .filter_map(|value| u64::try_from(value).ok())
        .filter_map(|value| (value != 0).then(|| SpaceId::new(value)))
        .collect()
}

pub fn window_space(id: WindowServerId) -> Option<SpaceId> {
    let spaces = window_spaces(id);
    // SLSCopySpacesForWindows can return multiple space IDs for a window during
    // Mission Control or fullscreen transitions — the window's real home space plus
    // a transient fullscreen space. Prefer any user space (type 0) in the list so
    // that Desktop windows are not misidentified as belonging to a fullscreen space.
    spaces
        .iter()
        .copied()
        .find(|s| unsafe { SLSSpaceGetType(*G_CONNECTION, s.get()) } == 0)
        .or_else(|| spaces.into_iter().next())
}

/// Whether the window server has this window ordered in.
///
/// `None` means unanswerable, and `Some(false)` retires the window, so a test with no override gets `None`.
/// See "A unit test must not read the live window server" in `docs/testing.md`.
#[cfg(test)]
pub fn window_ordered_in(id: WindowServerId) -> Option<bool> {
    TEST_WINDOW_ORDERED_IN_OVERRIDE
        .with(|override_ordered| override_ordered.borrow().get(&id.as_u32()).copied())
}

#[cfg(not(test))]
pub fn window_ordered_in(id: WindowServerId) -> Option<bool> {
    let mut ordered: u8 = 0;
    if let Ok(_) = cg_ok(unsafe { SLSWindowIsOrderedIn(*G_CONNECTION, id.as_u32(), &mut ordered) })
    {
        return Some(ordered != 0);
    }

    None
}

pub fn window_is_ordered_in(id: WindowServerId) -> bool { window_ordered_in(id).unwrap_or(false) }

pub fn get_windows_raw<T: Type>(
    options: CGWindowListOption,
    relative_to_window: CGWindowID,
) -> CFRetained<CFArray<T>> {
    unsafe {
        // TODO: cgwindowlistcopywindowinfo does not appear to order windows properly
        // SAFETY: this will almost always return (pre objc2 was not a result and just a cfarray)
        if let Some(windows) = CGWindowListCopyWindowInfo(options, relative_to_window) {
            CFRetained::cast_unchecked(windows)
        } else {
            CFArray::empty()
        }
    }
}

pub fn get_visible_windows_raw<T: Type>() -> CFRetained<CFArray<T>> {
    get_windows_raw(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        kCGNullWindowID,
    )
}

#[cfg(test)]
pub fn get_windows(ids: &[WindowServerId]) -> Vec<WindowServerInfo> {
    ids.iter()
        .map(|&id| WindowServerInfo {
            id,
            pid: 1234,
            layer: 0,
            frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(800.0, 600.0)),
            min_frame: CGSize::ZERO,
            max_frame: CGSize::ZERO,
        })
        .collect()
}

#[cfg(not(test))]
pub fn get_windows(ids: &[WindowServerId]) -> Vec<WindowServerInfo> {
    let Some(query) = WindowIterator::new(ids) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(ids.len());
    while query.advance().is_some() {
        if let Some(info) = window_info_from_query(&query) {
            out.push(info);
        }
    }
    out
}

#[cfg(test)]
pub fn get_window(id: WindowServerId) -> Option<WindowServerInfo> {
    get_windows(&[id]).into_iter().next()
}

/// A single-window query rather than `get_windows(&[id])`, which is the same answer through a
/// general path. The count check is the reason: asking for one window and being handed a different
/// number means the id is not the window it names any more, and a recycled id must not be answered.
#[cfg(not(test))]
pub fn get_window(id: WindowServerId) -> Option<WindowServerInfo> {
    let query = WindowIterator::new(&[id])?;
    if query.count() != 1 || query.advance().is_none() {
        return None;
    }
    window_info_from_query(&query)
}

pub fn get_num(dict: &CFDictionary<CFString, CFType>, key: &'static CFString) -> Option<i64> {
    dict.get(key)?.downcast::<CFNumber>().ok()?.as_i64()
}

/// Reads a `kCGWindowBounds` dictionary, whose keys are the plain strings X, Y, Width and Height.
///
/// Parsed by hand because the geometry helper that would do this is not exposed by the bindings in
/// use. Returns `None` if any component is missing, since a partially read frame would place a
/// window somewhere arbitrary rather than fail visibly.
pub fn bounds_from_dict(dict: CFRetained<CFDictionary>) -> Option<CGRect> {
    // The untyped CFDictionary carries no key or value types, so retype it the same way
    // get_windows_raw does before reading it.
    // SAFETY: a kCGWindowBounds dictionary has CFString keys and CFNumber values.
    let dict: CFRetained<CFDictionary<CFString, CFType>> =
        unsafe { CFRetained::cast_unchecked(dict) };
    let component = |name: &str| -> Option<f64> {
        let key = CFString::from_str(name);
        dict.get(&key)?.downcast::<CFNumber>().ok()?.as_f64()
    };
    Some(CGRect::new(
        CGPoint::new(component("X")?, component("Y")?),
        CGSize::new(component("Width")?, component("Height")?),
    ))
}

/// Do two rects share any area?
pub fn overlaps(a: CGRect, b: CGRect) -> bool {
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}

/// Front-to-back position of every on-screen window, 0 being frontmost.
/// `CGWindowListCopyWindowInfo` lists on-screen windows front to back, so the index is the depth.
#[cfg(not(test))]
pub fn front_to_back_depths() -> std::collections::HashMap<u32, usize> {
    get_visible_windows_raw::<CFDictionary<CFString, CFType>>()
        .iter()
        .filter_map(|window| get_num(&window, unsafe { kCGWindowNumber }).map(|id| id as u32))
        .enumerate()
        .map(|(depth, id)| (id, depth))
        .collect()
}

/// See "A unit test must not read the live window server" in `docs/testing.md`. Empty until a test
/// sets the order with `set_front_to_back_override`.
#[cfg(test)]
pub fn front_to_back_depths() -> std::collections::HashMap<u32, usize> {
    TEST_FRONT_TO_BACK_OVERRIDE
        .with(|order| order.borrow().iter().enumerate().map(|(depth, id)| (*id, depth)).collect())
}

/// The on-screen order for test builds, frontmost first. `None` clears it.
#[cfg(test)]
pub fn set_front_to_back_override(order: Option<Vec<u32>>) {
    TEST_FRONT_TO_BACK_OVERRIDE.with(|current| *current.borrow_mut() = order.unwrap_or_default());
}

/// Layer-0 windows on screen and intersecting `display`, with their frames. Other layers hold the
/// bar, Dock and notifications. Parked slivers are filtered: on screen to CoreGraphics, no pixels.
pub fn visible_windows_on_display(display: CGRect) -> Vec<(WindowServerId, CGRect)> {
    /// Below this in either axis a window is a parked strip rather than something worth drawing.
    const MIN_SIDE: f64 = 100.0;

    get_visible_windows_raw::<CFDictionary<CFString, CFType>>()
        .iter()
        .filter_map(|window| {
            if get_num(&window, unsafe { kCGWindowLayer }) != Some(0) {
                return None;
            }
            let id = get_num(&window, unsafe { kCGWindowNumber })? as u32;
            let bounds = window
                .get(unsafe { kCGWindowBounds })?
                .downcast::<CFDictionary>()
                .ok()
                .and_then(bounds_from_dict)?;
            if bounds.size.width < MIN_SIDE || bounds.size.height < MIN_SIDE {
                return None;
            }
            if !overlaps(bounds, display) {
                return None;
            }
            Some((WindowServerId::new(id), bounds))
        })
        .collect()
}

#[cfg_attr(test, allow(dead_code))]
fn window_is_effectively_invisible(alpha: f32, layer: i32) -> bool {
    layer == 0 && alpha <= EFFECTIVELY_INVISIBLE_WINDOW_ALPHA
}

#[cfg_attr(test, allow(dead_code))]
fn window_info_from_query(query: &WindowIterator) -> Option<WindowServerInfo> {
    let layer = query.level();
    if window_is_effectively_invisible(query.alpha(), layer) {
        return None;
    }
    let (min_frame, max_frame) = query.constraints();
    Some(WindowServerInfo {
        id: WindowServerId::new(query.window_id()),
        pid: query.pid() as i32,
        layer,
        frame: query.bounds(),
        min_frame,
        max_frame,
    })
}

/// Find the topmost window at `point`, or the next window below
/// `below_window_id` when given. Returns `(window_id, owner_connection_id)`,
/// or `None` when no window is found.
fn find_window_at_point(point: &mut CGPoint, below_window_id: Option<u32>) -> Option<(u32, i32)> {
    let mut window_point = CGPoint { x: 0.0, y: 0.0 };
    let (mut wid, mut wcid) = (0u32, 0i32);

    let (start_id, direction) = match below_window_id {
        Some(id) => (id as i32, -1),
        None => (0, 1),
    };

    unsafe {
        SLSFindWindowAndOwner(
            *G_CONNECTION,
            start_id,
            direction,
            0,
            point,
            &mut window_point,
            &mut wid,
            &mut wcid,
        );
    }

    (wid != 0).then_some((wid, wcid))
}

fn is_own_window(cid: i32) -> bool { *G_CONNECTION == cid }

pub fn get_window_at_point(mut point: CGPoint) -> Option<WindowServerId> {
    let (mut wid, cid) = find_window_at_point(&mut point, None)?;
    if is_own_window(cid) {
        wid = find_window_at_point(&mut point, Some(wid))?.0;
    }
    Some(WindowServerId(wid))
}

/// Returns `true` if an external application window at normal level or above
/// occludes the given screen point.
///
/// Walks down the window stack at `point`, skipping all Rini-owned CGS
/// windows (there may be more than one at the same point), until a non-Rini
/// window is found. Desktop/wallpaper windows sit well below
/// `NSNormalWindowLevel` and are not considered occluders.
pub fn is_point_occluded_by_external_window(mut point: CGPoint) -> bool {
    use objc2_app_kit::NSNormalWindowLevel;

    let mut hit = find_window_at_point(&mut point, None);

    // Skip past any Rini-owned windows stacked at this point.
    while let Some((wid, cid)) = hit {
        if !is_own_window(cid) {
            let level = window_level(wid).unwrap_or(NSWindowLevel::MIN);
            return level >= NSNormalWindowLevel;
        }
        hit = find_window_at_point(&mut point, Some(wid));
    }

    false
}

pub fn current_cursor_location() -> Result<CGPoint, CGError> {
    let mut point = CGPoint::new(0.0, 0.0);
    cg_ok(unsafe { SLSGetCurrentCursorLocation(*G_CONNECTION, &mut point) })?;
    Ok(point)
}

pub fn window_under_cursor() -> Option<WindowServerId> {
    let point = current_cursor_location().ok()?;
    get_window_at_point(point)
}

#[cfg(test)]
pub fn window_level(_wid: u32) -> Option<NSWindowLevel> { Some(0) }

#[cfg(not(test))]
pub fn window_level(wid: u32) -> Option<NSWindowLevel> {
    let query = WindowIterator::new(&[WindowServerId::new(wid)])?;
    Some(query.advance()?.level() as NSWindowLevel)
}

/// Returns the typed Skylight tags exposed by a window-query iterator.
#[cfg_attr(test, allow(dead_code))]
fn iterator_window_tags(iterator: *mut CFType) -> SLSWindowTags {
    SLSWindowTags::from_bits_retain(unsafe { SLSWindowIteratorGetTags(iterator) })
}

/// Returns whether the tags describe a document or floating app window.
#[cfg_attr(test, allow(dead_code))]
fn tags_match_app_window_role(tags: SLSWindowTags) -> bool {
    tags.contains(SLSWindowTags::DOCUMENT) || tags.contains(SLSWindowTags::FLOATING)
}

/// Returns whether the iterator points at a top-level application window.
#[cfg_attr(test, allow(dead_code))]
pub fn iterator_window_suitable(iterator: *mut CFType) -> bool {
    let tags = iterator_window_tags(iterator);
    let parent_wid = unsafe { SLSWindowIteratorGetParentID(iterator) };
    parent_wid == 0 && tags_match_app_window_role(tags)
}

/// The windows the window server reports on `spaces`.
///
/// Callers treat this as authoritative membership, so a test answers from its overrides only and gets
/// nothing when none is set. See "A unit test must not read the live window server" in `docs/testing.md`.
pub fn space_window_list_for_connection(
    spaces: &[u64],
    owner: u32,
    include_minimized: bool,
) -> Vec<u32> {
    #[cfg(test)]
    {
        if spaces.len() == 1
            && let Some(override_ids) = TEST_SPACE_WINDOW_LIST_BY_SPACE_OVERRIDE
                .with(|ids| ids.borrow().get(&spaces[0]).cloned())
        {
            let _ = (owner, include_minimized);
            return override_ids;
        }
        if let Some(override_ids) = TEST_SPACE_WINDOW_LIST_OVERRIDE.with(|ids| ids.borrow().clone())
        {
            let _ = (spaces, owner, include_minimized);
            return override_ids;
        }
        Vec::new()
    }
    #[cfg(not(test))]
    space_window_list_from_window_server(spaces, owner, include_minimized)
}

// credit to yabai
#[cfg(not(test))]
fn space_window_list_from_window_server(
    spaces: &[u64],
    owner: u32,
    include_minimized: bool,
) -> Vec<u32> {
    let cf_space_array = cf_array_from_u64s(spaces);

    let mut set_tags: u64 = 0;
    let mut clear_tags: u64 = 0;
    let options: u32 = if include_minimized { 0x7 } else { 0x2 };

    let window_list_ref = unsafe {
        SLSCopyWindowsWithOptionsAndTags(
            *G_CONNECTION,
            owner,
            CFRetained::as_ptr(&cf_space_array).as_ptr(),
            options,
            &mut set_tags,
            &mut clear_tags,
        )
    };

    if window_list_ref.is_null() {
        return Vec::new();
    }

    let expected = (unsafe { &*window_list_ref }).len() as i32;
    if expected == 0 {
        unsafe { CFRelease(window_list_ref as *mut CFType) };
        return Vec::new();
    }

    let iterator = WindowIterator::new_from_cfarray(window_list_ref, 0);
    unsafe { CFRelease(window_list_ref as *mut CFType) };
    let Some(iterator) = iterator else {
        return Vec::new();
    };

    let mut windows = Vec::with_capacity(expected as usize);

    while iterator.advance().is_some() {
        let tags = iterator_window_tags(iterator.iter);
        let parent_id = iterator.parent_id();
        let wid = iterator.window_id();
        let is_candidate = parent_id == 0 && tags_match_app_window_role(tags);

        if is_candidate {
            windows.push(wid);
        }
    }

    windows
}

/// The key window on `space` per the window server. The key-focus process can briefly differ from
/// the frontmost one, and this does not wait on the app's `AXMainWindow`.
pub fn key_focused_window(space: SpaceId) -> Option<WindowId> {
    let mut psn = ProcessSerialNumber::default();
    let mut fallback = 0u8;
    if cg_ok(unsafe { SLPSGetKeyFocusProcess(&mut psn, &mut fallback) }).is_err() {
        return None;
    }

    let mut owner = 0;
    if unsafe { SLSGetConnectionIDForPSN(*G_CONNECTION, &psn, &mut owner) } != 0 || owner == 0 {
        return None;
    }

    let filter = WindowQueryFilter {
        owner,
        spaces: &[space.get()],
        space_list_options: 0,
        window_list_options: 0x2,
        query_flags: 0x2,
        include_tags: SLSWindowTags::DOCUMENT.bits(),
        exclude_tags: (SLSWindowTags::ON_ALL_WORKSPACES
            | SLSWindowTags::HIDDEN
            | SLSWindowTags::MINIATURIZED)
            .bits(),
    };
    let query = window_query_run(&filter)?;
    query.advance()?;
    let wsid = WindowServerId::new(query.window_id());

    Some(WindowId {
        pid: query.pid(),
        idx: wsid.as_nonzero()?,
    })
}

#[cfg(test)]
pub fn set_space_window_list_for_connection_override(ids: Option<Vec<u32>>) {
    TEST_SPACE_WINDOW_LIST_OVERRIDE.with(|override_ids| *override_ids.borrow_mut() = ids);
}

#[cfg(test)]
pub fn set_space_window_list_for_space_override(space: u64, ids: Option<Vec<u32>>) {
    TEST_SPACE_WINDOW_LIST_BY_SPACE_OVERRIDE.with(|override_ids| {
        let mut override_ids = override_ids.borrow_mut();
        if let Some(ids) = ids {
            override_ids.insert(space, ids);
        } else {
            override_ids.remove(&space);
        }
    });
}

#[cfg(test)]
pub fn set_window_spaces_override(id: WindowServerId, spaces: Option<Vec<u64>>) {
    TEST_WINDOW_SPACES_OVERRIDE.with(|override_spaces| {
        let mut override_spaces = override_spaces.borrow_mut();
        if let Some(spaces) = spaces {
            override_spaces.insert(id.as_u32(), spaces);
        } else {
            override_spaces.remove(&id.as_u32());
        }
    });
}

#[cfg(test)]
pub fn set_window_ordered_in_override(id: WindowServerId, ordered: Option<bool>) {
    TEST_WINDOW_ORDERED_IN_OVERRIDE.with(|override_ordered| {
        let mut override_ordered = override_ordered.borrow_mut();
        if let Some(ordered) = ordered {
            override_ordered.insert(id.as_u32(), ordered);
        } else {
            override_ordered.remove(&id.as_u32());
        }
    });
}

/// Whether the window server considers this a top-level application window.
///
/// `None` means unanswerable, which is no evidence either way, while `Some(false)` retires the window. A
/// test gets `None`. See "A unit test must not read the live window server" in `docs/testing.md`.
#[cfg(test)]
pub fn app_window_suitability(_id: WindowServerId) -> Option<bool> { None }

#[cfg(not(test))]
pub fn app_window_suitability(id: WindowServerId) -> Option<bool> {
    let query = WindowIterator::new(&[id])?;

    if query.count() > 0 && query.advance().is_some() {
        Some(iterator_window_suitable(query.iter))
    } else {
        Some(false)
    }
}

pub fn app_window_suitable(id: WindowServerId) -> bool {
    app_window_suitability(id).unwrap_or(false)
}

// credit: https://github.com/Hammerspoon/hammerspoon/issues/370#issuecomment-545545468
pub fn make_key_window(pid: pid_t, wsid: WindowServerId) -> Result<(), CGError> {
    #[allow(non_upper_case_globals)]
    const kCPSUserGenerated: u32 = 0x200;

    let mut event1 = [0u8; 0x100];
    event1[0x04] = 0xf8;
    event1[0x08] = 0x01;
    event1[0x3a] = 0x10;
    event1[0x3c..0x40].copy_from_slice(&wsid.0.to_le_bytes());
    event1[0x20..0x30].fill(0xff);

    let mut event2 = event1;
    event2[0x08] = 0x02;

    let psn = crate::windows::platform::process::psn_for_pid(pid)?;

    unsafe {
        cg_ok(_SLPSSetFrontProcessWithOptions(&psn, wsid.0, kCPSUserGenerated))?;
        cg_ok(SLPSPostEventRecordTo(&psn, event1.as_ptr()))?;
        cg_ok(SLPSPostEventRecordTo(&psn, event2.as_ptr()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

    use super::{
        WindowServerId, app_window_suitability, overlaps, set_window_ordered_in_override,
        space_window_list_for_connection, window_ordered_in, window_spaces,
    };

    /// A query with no override must answer "unknown", never the truth about the developer's screen.
    ///
    /// An unanswerable query is no evidence, while a negative one retires the window. See "A unit test must
    /// not read the live window server" in `docs/testing.md`.
    #[test]
    fn a_test_never_reads_the_live_window_server() {
        let id = WindowServerId::new(10001);
        assert_eq!(app_window_suitability(id), None);
        assert_eq!(window_ordered_in(id), None);
        assert!(window_spaces(id).is_empty());
        // Space 1 is the space every reactor test uses, and the one a real machine is most likely to have.
        assert!(space_window_list_for_connection(&[1], 0, false).is_empty());
    }

    #[test]
    fn an_override_is_still_honoured() {
        let id = WindowServerId::new(10002);
        set_window_ordered_in_override(id, Some(false));
        assert_eq!(window_ordered_in(id), Some(false));
        set_window_ordered_in_override(id, None);
        assert_eq!(window_ordered_in(id), None);
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn overlapping_rects_overlap() {
        assert!(overlaps(
            rect(0.0, 0.0, 100.0, 100.0),
            rect(50.0, 50.0, 100.0, 100.0)
        ));
    }

    #[test]
    fn a_window_fully_inside_a_display_overlaps_it() {
        let display = rect(0.0, 32.0, 1728.0, 1085.0);
        assert!(overlaps(rect(865.0, 32.0, 859.0, 1081.0), display));
    }

    #[test]
    fn a_window_scrolled_entirely_off_the_left_does_not_overlap() {
        // Real measured geometry: off-strip columns sit at x = -1680 with width 1720, so their right
        // edge lands at 40 and they DO still touch the display. A window fully clear of it must not.
        let display = rect(0.0, 32.0, 1728.0, 1085.0);
        assert!(!overlaps(rect(-2000.0, 32.0, 859.0, 1081.0), display));
    }

    #[test]
    fn a_window_overlapping_by_its_parked_sliver_still_counts_as_overlapping() {
        // The parked case, which the caller filters on size rather than on overlap.
        let display = rect(0.0, 32.0, 1728.0, 1085.0);
        assert!(overlaps(rect(-1680.0, 32.0, 1720.0, 1081.0), display));
    }

    #[test]
    fn a_window_on_the_next_display_does_not_overlap_this_one() {
        let display = rect(0.0, 32.0, 1728.0, 1085.0);
        assert!(!overlaps(rect(1728.0, 32.0, 859.0, 1081.0), display));
    }

    #[test]
    fn merely_touching_edges_does_not_count_as_overlap() {
        // Exclusive comparison, so a window whose right edge is exactly the display's left edge
        // contributes no visible pixels and is excluded.
        let display = rect(0.0, 0.0, 100.0, 100.0);
        assert!(!overlaps(rect(-50.0, 0.0, 50.0, 100.0), display));
    }

    #[test]
    fn zero_window_server_id_is_not_a_window_id() {
        assert!(WindowServerId::new(0).as_nonzero().is_none());
        assert_eq!(WindowServerId::new(42).as_nonzero().map(|id| id.get()), Some(42));
    }
}

/// Computes whether a window is manageable based on its properties and window server information.
///
/// A window is manageable if:
/// - It is not minimized
/// - Its layer is 0 (if info available)
/// - It is not sticky
/// - Its level is normal (if available)
/// - It is AX standard and AX root
pub fn compute_window_manageability(
    window_server_id: Option<WindowServerId>,
    is_minimized: bool,
    is_ax_standard: bool,
    is_ax_root: bool,
    mut window_server_info: impl FnMut(WindowServerId) -> Option<WindowServerInfo>,
) -> bool {
    if is_minimized {
        return false;
    }

    if let Some(wsid) = window_server_id {
        if let Some(info) = window_server_info(wsid) {
            if info.layer != 0 {
                return false;
            }
        }
        if window_is_sticky(wsid) {
            return false;
        }

        if let Some(level) = window_level(wsid.0) {
            if level != NSNormalWindowLevel {
                return false;
            }
        }
    }
    is_ax_standard && is_ax_root
}

/// Whether the window server's picture of a window the app no longer lists says it is gone:
/// not a suitable window, off layer 0, too small to be real, or ordered out. A failed query
/// (`None`) is not evidence; only an explicit negative observation retires a window.
pub fn looks_gone(
    info: &WindowServerInfo,
    suitable: Option<bool>,
    ordered_in: Option<bool>,
) -> bool {
    const MIN_REAL_WINDOW_DIMENSION: f64 = 2.0;
    let too_small = info.frame.size.width.abs() < MIN_REAL_WINDOW_DIMENSION
        || info.frame.size.height.abs() < MIN_REAL_WINDOW_DIMENSION;
    suitable == Some(false) || info.layer != 0 || too_small || ordered_in == Some(false)
}

/// A window's place in the window server's stack: its frame, level and sub-level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StackPlace {
    pub frame: CGRect,
    pub level: Option<NSWindowLevel>,
    pub sub_level: c_int,
}

impl StackPlace {
    pub fn of(wsid: WindowServerId, frame: CGRect) -> Self {
        let id = wsid.as_u32();
        Self {
            frame,
            level: window_level(id),
            sub_level: window_sub_level(id),
        }
    }
}

/// Whether one of the windows stacked above `candidate` sits wholly inside its frame at the same
/// level and sub-level. Raising the candidate would put it over that window, which the user has
/// deliberately on top; `above` is the stack from the top down to the candidate, exclusive.
pub fn covered_by_peer_above(
    candidate: StackPlace,
    above: impl IntoIterator<Item = StackPlace>,
) -> bool {
    above.into_iter().any(|peer| {
        candidate.frame.contains_rect(peer.frame)
            && candidate.level.zip(peer.level).is_some_and(|(c, p)| c == p)
            && candidate.sub_level == peer.sub_level
    })
}

#[cfg(test)]
mod stack_tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn place(x: f64, w: f64, level: Option<NSWindowLevel>, sub_level: c_int) -> StackPlace {
        StackPlace {
            frame: CGRect::new(CGPoint::new(x, 0.0), CGSize::new(w, 100.0)),
            level,
            sub_level,
        }
    }

    #[test]
    fn a_peer_wholly_inside_at_the_same_level_covers() {
        let candidate = place(0.0, 1000.0, Some(0), 0);
        assert!(covered_by_peer_above(candidate, [place(
            100.0,
            200.0,
            Some(0),
            0
        )]));
    }

    #[test]
    fn a_peer_at_another_level_or_sub_level_or_overhanging_does_not() {
        let candidate = place(0.0, 1000.0, Some(0), 0);
        assert!(!covered_by_peer_above(candidate, [place(
            100.0,
            200.0,
            Some(3),
            0
        )]));
        assert!(!covered_by_peer_above(candidate, [place(
            100.0,
            200.0,
            Some(0),
            1
        )]));
        assert!(
            !covered_by_peer_above(candidate, [place(900.0, 200.0, Some(0), 0)]),
            "overhangs the edge"
        );
        assert!(!covered_by_peer_above(candidate, []));
    }

    fn info(layer: i32, w: f64, h: f64) -> WindowServerInfo {
        WindowServerInfo {
            id: WindowServerId::new(1),
            pid: 1,
            layer,
            frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h)),
            min_frame: CGSize::new(0.0, 0.0),
            max_frame: CGSize::new(0.0, 0.0),
        }
    }

    #[test]
    fn a_window_is_gone_only_on_an_explicit_negative_observation() {
        let real = info(0, 800.0, 600.0);
        assert!(
            !looks_gone(&real, None, None),
            "unknown suitability and order are not evidence"
        );
        assert!(!looks_gone(&real, Some(true), Some(true)));
        assert!(looks_gone(&real, Some(false), None));
        assert!(looks_gone(&real, None, Some(false)));
        assert!(looks_gone(&info(1, 800.0, 600.0), None, None), "off layer 0");
        assert!(
            looks_gone(&info(0, 1.0, 600.0), None, None),
            "a sliver is not a window"
        );
    }

    #[test]
    fn an_unknown_level_never_covers() {
        let candidate = place(0.0, 1000.0, None, 0);
        assert!(!covered_by_peer_above(candidate, [place(100.0, 200.0, None, 0)]));
    }
}
