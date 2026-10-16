//! The window server, through SkyLight: the FFI reads and writes behind the [`WindowServer`] port,
//! answered for this machine by [`LiveWindowServer`].
use std::ffi::{CStr, c_int};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use objc2_application_services::AXError;
use objc2_core_foundation::{
    CFArray, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize, Type,
};
use objc2_core_graphics::CGError;
use once_cell::sync::Lazy;
use rini_core::ids::{DisplayUuid, SpaceId, WindowId, WindowServerId, pid_t};
use rini_skylight_sys::*;

#[cfg(test)]
mod fake;
mod port;

#[cfg(test)]
pub use fake::FakeWindowServer;
pub use port::{Command, LiveWindowServer, WindowServer, live};

use crate::windows::domain::info::{WindowServerInfo, reported_radius};
use crate::windows::domain::stack::{StackWrite, WindowLevel, stack_place_writes};
use crate::windows::platform::ax::element::{AXUIElement, Error as AxError};
use crate::windows::platform::process::ProcessSerialNumber;
use crate::windows::platform::sub_level::window_sub_level;
use crate::windows::platform::{cg_ok, symbol};

/// A connection of rini's own that no window of rini's belongs to, for every read that waits on the
/// connection's last Core Animation transaction. On the main one such a read deadlocks with the main
/// thread's commit for 500ms ("Reads go on a connection of their own" in `src/windows/docs/README.md`).
/// The main connection is made first: the first `SLSNewConnection` in a process becomes the main one.
pub static READ_CONNECTION: Lazy<i32> = Lazy::new(|| {
    let main = *G_CONNECTION;
    let mut cid = 0;
    // SAFETY: `cid` is a local the call writes the new connection's id into.
    let err = unsafe { SLSNewConnection(0, &mut cid) };
    let read = opened_or(err, cid, main);
    if read == main {
        tracing::warn!(
            ?err,
            cid,
            "no SkyLight connection of rini's own; window reads share the main one"
        );
    }
    read
});

/// The connection `SLSNewConnection` answered `err` and `cid` for, or `main` when it opened none of
/// rini's own: an error, no id, or the main connection's id.
fn opened_or(err: CGError, cid: i32, main: i32) -> i32 {
    if err == CGError::Success && cid != 0 && cid != main {
        cid
    } else {
        main
    }
}
static LAST_WINDOWSERVER_ACTIVITY_US: AtomicU64 = AtomicU64::new(0);

pub const WINDOWSERVER_QUIET_US: u64 = 350_000;
const EFFECTIVELY_INVISIBLE_WINDOW_ALPHA: f32 = 0.01;

impl TryFrom<&AXUIElement> for WindowServerId {
    type Error = AxError;

    fn try_from(element: &AXUIElement) -> Result<Self, Self::Error> {
        let mut id = 0;
        // SAFETY: `element` holds the AXUIElement alive for the call, and `id` is a local it writes.
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
    // Keep this trace low-cost; only display churn's settling reads the activity.
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

/// A SkyLight window iterator: a cursor over a query's windows, each read about the one it stands on.
pub struct WindowIterator {
    /// Non-null, owned +1 from `SLSWindowQueryResultCopyWindows`, and released on drop.
    iter: *mut CFType,
}

/// Reads of the window a [`WindowIterator`] stands on, one `SLSWindowIterator*` call each.
macro_rules! iterator_reads {
    ($($vis:vis $name:ident: $read:ident -> $out:ty;)*) => {$(
        #[inline]
        $vis fn $name(&self) -> $out {
            // SAFETY: `self.iter` is the live iterator this owns, and the read takes only the
            // iterator and returns a plain value.
            unsafe { $read(self.iter) }
        }
    )*};
}

impl WindowIterator {
    iterator_reads! {
        pub count: SLSWindowIteratorGetCount -> i32;
        stepped: SLSWindowIteratorAdvance -> bool;
        pub window_id: SLSWindowIteratorGetWindowID -> u32;
        pub level: SLSWindowIteratorGetLevel -> i32;
        pub pid: SLSWindowIteratorGetPID -> i32;
        pub parent_id: SLSWindowIteratorGetParentID -> u32;
        pub bounds: SLSWindowIteratorGetBounds -> CGRect;
        pub alpha: SLSWindowIteratorGetAlpha -> f32;
        pub attributes: SLSWindowIteratorGetAttributes -> u64;
        tag_bits: SLSWindowIteratorGetTags -> u64;
    }

    pub fn new(ids: &[WindowServerId]) -> Option<Self> {
        if ids.is_empty() {
            return None;
        }
        Self::new_from_cfarray(&cf_array_from_ids(ids), 0)
    }

    /// `flags` controls optional result decoding; bit 0 requests window titles.
    fn new_from_cfarray(cf_numbers: &CFArray<CFNumber>, flags: c_int) -> Option<Self> {
        let cf_numbers = NonNull::from(cf_numbers).as_ptr();
        // SAFETY: `cf_numbers` is a live CFArray of CFNumbers, borrowed for the call.
        let query = unsafe { SLSWindowQueryWindows(*READ_CONNECTION, cf_numbers, flags) };
        if query.is_null() {
            return None;
        }
        // SAFETY: `query` is the non-null query result just made; the copy is +1 to us.
        let iter = unsafe { SLSWindowQueryResultCopyWindows(query) };
        // SAFETY: `query` came from a Create call, is ours, and is not used again.
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

    /// The iterator moved on to its next window, none when it has no more.
    #[inline]
    pub fn advance(&self) -> Option<&Self> { self.stepped().then_some(self) }

    /// The current window's title, from a query asked with flag bit 0.
    pub fn title(&self) -> Option<String> {
        // SAFETY: `self.iter` is the live iterator this owns.
        let title = NonNull::new(unsafe { SLSWindowIteratorCopyTitle(self.iter) })?;
        // SAFETY: a Copy call, so the title is +1 and the `CFRetained` takes that retain.
        Some(unsafe { CFRetained::from_raw(title) }.to_string())
    }

    /// The current window's SkyLight tags.
    fn tags(&self) -> SLSWindowTags { SLSWindowTags::from_bits_retain(self.tag_bits()) }

    /// Whether the current window is a top-level application window.
    fn is_app_window(&self) -> bool {
        self.parent_id() == 0 && tags_match_app_window_role(self.tags())
    }

    #[inline]
    pub fn constraints(&self) -> (CGSize, CGSize) {
        let mut min = CGSize::ZERO;
        let mut max = CGSize::ZERO;
        let mut cur = CGSize::ZERO;
        // SAFETY: `self.iter` is the live iterator this owns, and the three sizes are locals the
        // call writes.
        unsafe { SLSWindowIteratorGetConstraints(self.iter, &mut min, &mut max, &mut cur) };

        if min.width == 0.0 && min.height == 0.0 && max.width == 0.0 && max.height == 0.0 {
            // SAFETY: the three sizes are locals the call writes.
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

    /// The current window's corner radii in points, as the window server reports them: empty when
    /// it answers none or has no such call.
    pub fn corner_radii(&self) -> Vec<f64> {
        let Some(read) = *GET_CORNER_RADII else {
            return Vec::new();
        };
        // SAFETY: `read` is SkyLight's `SLSWindowIteratorGetCornerRadii`, given the live iterator
        // this owns.
        let Some(radii) = NonNull::new(unsafe { read(self.iter) }) else {
            return Vec::new();
        };
        // SAFETY: the call answers a new CFArray of CFNumbers at +1 despite its Get name, measured
        // in `crates/rini-skylight-sys/docs/README.md`, and the `CFRetained` takes that +1.
        let radii = unsafe { CFRetained::from_raw(radii) };
        radii.iter().filter_map(|corner| corner.as_f64()).collect()
    }
}

impl Drop for WindowIterator {
    fn drop(&mut self) {
        // SAFETY: `self.iter` is the +1 this owns, and nothing uses it after the drop.
        unsafe { CFRelease(self.iter) }
    }
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

/// The SkyLight query keys rini sets, each the name of an exported `CFStringRef` global.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowQueryKey {
    Owner,
    Spaces,
    SpaceListOptions,
    WorkspaceWindowListOptions,
    IncludeTags,
    ExcludeTags,
}

impl WindowQueryKey {
    const fn symbol_name(self) -> &'static CStr {
        match self {
            Self::Owner => c"SLSWindowQueryKeyOwner",
            Self::Spaces => c"SLSWindowQueryKeySpaces",
            Self::SpaceListOptions => c"SLSWindowQueryKeySpaceListOptions",
            Self::WorkspaceWindowListOptions => c"SLSWindowQueryKeyWorkspaceWindowListOptions",
            Self::IncludeTags => c"SLSWindowQueryKeyIncludeTags",
            Self::ExcludeTags => c"SLSWindowQueryKeyExcludeTags",
        }
    }

    fn resolve(self) -> Option<usize> {
        let slot = symbol(self.symbol_name())?;
        // SAFETY: every `symbol_name` is a SkyLight data symbol holding a `CFStringRef`, so `slot`
        // addresses a readable, pointer-sized global.
        let key = unsafe { *(slot.as_ptr().cast::<*mut CFString>()) };
        (!key.is_null()).then_some(key as usize)
    }
}

static GET_CORNER_RADII: Lazy<Option<SLSWindowIteratorGetCornerRadiiFn>> = Lazy::new(|| {
    let function = symbol(c"SLSWindowIteratorGetCornerRadii")?;
    // SAFETY: the symbol is SkyLight's function of this signature; no cast to a fn pointer is checked.
    Some(unsafe {
        std::mem::transmute::<*mut std::ffi::c_void, SLSWindowIteratorGetCornerRadiiFn>(
            function.as_ptr(),
        )
    })
});

static WINDOW_QUERY_KEYS: Lazy<Option<WindowQueryKeys>> = Lazy::new(|| {
    Some(WindowQueryKeys {
        owner: WindowQueryKey::Owner.resolve()?,
        spaces: WindowQueryKey::Spaces.resolve().unwrap_or(0),
        space_options: WindowQueryKey::SpaceListOptions.resolve().unwrap_or(0),
        // This key appeared later than the core query API. A missing value is
        // represented by zero and simply leaves the server default in place.
        window_options: WindowQueryKey::WorkspaceWindowListOptions.resolve().unwrap_or(0),
        include_tags: WindowQueryKey::IncludeTags.resolve()?,
        exclude_tags: WindowQueryKey::ExcludeTags.resolve()?,
    })
});

/// # Safety
///
/// `query` is a live query from `SLSWindowQueryCreate`, and `key`, when not 0, a query key resolved
/// by `WindowQueryKey::resolve` that takes a value of `T`'s CF type.
unsafe fn set_window_query_value<T: Type>(query: *mut CFType, key: usize, value: &CFRetained<T>) {
    if key != 0 {
        // SAFETY: the caller's; `value` is a live CF object, which the query retains if it keeps it.
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

    // SAFETY: a null dictionary asks for an empty query, which the call answers +1.
    let query = unsafe { SLSWindowQueryCreate(std::ptr::null_mut()) };
    if query.is_null() {
        return None;
    }

    let owner = CFNumber::new_i32(filter.owner);
    let include_tags = CFNumber::new_i64(filter.include_tags as i64);
    let exclude_tags = CFNumber::new_i64(filter.exclude_tags as i64);
    let window_options =
        (keys.window_options != 0).then(|| CFNumber::new_i32(filter.window_list_options));
    // SAFETY: `query` is the live query made above, and each key takes a CFNumber.
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
    // SAFETY: `query` is live; the spaces key takes a CFArray of CFNumbers, the options key a
    // CFNumber.
    unsafe {
        if let Some(space_array) = &space_array {
            set_window_query_value(query, keys.spaces, space_array);
        } else if let Some(space_options) = &space_options {
            set_window_query_value(query, keys.space_options, space_options);
        }
    }

    // SAFETY: `query` is the live query made above; the result is +1 to us.
    let result = unsafe { SLSWindowQueryRun(*READ_CONNECTION, query, filter.query_flags) };
    let iterator = if result.is_null() {
        std::ptr::null_mut()
    } else {
        // SAFETY: `result` is the non-null result just run; the copy is +1 to us.
        unsafe { SLSWindowQueryResultCopyWindows(result) }
    };
    // SAFETY: `result`, when made, and `query` are ours, +1, and not used again.
    unsafe {
        if !result.is_null() {
            CFRelease(result);
        }
        CFRelease(query);
    }
    WindowIterator::from_owned_iterator(iterator)
}

/// The iterator attribute set on a window that is ordered in: it agreed with `SLSWindowIsOrderedIn`,
/// which waits on the main connection whatever it is given, for all 177 windows asked ("Which reads
/// wait, and on which connection" in `src/windows/docs/window-server-reads.md`).
const ORDERED_IN_ATTRIBUTE: u64 = 1 << 1;

/// Whether a window whose iterator attributes are `attributes` is ordered in.
fn attributes_ordered_in(attributes: u64) -> bool { attributes & ORDERED_IN_ATTRIBUTE != 0 }

/// A window as a listing of the window server names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub id: WindowServerId,
    pub level: i32,
    pub pid: pid_t,
    pub bounds: CGRect,
    /// Its title, when the listing asked for titles and the window has one.
    pub title: Option<String>,
}

/// `kCGDesktopIconWindowLevel`, the Finder's desktop icons: a window at it or below is a desktop
/// element, which a list of every window on screen keeps ("The read connection answers as the main
/// one does" in `src/windows/docs/window-server-reads.md`).
const DESKTOP_ICON_LEVEL: i32 = -2147483603;

/// Whether a window at `level` is part of the desktop: its icons, the wallpaper, the backstop.
pub fn is_desktop_element(level: i32) -> bool { level <= DESKTOP_ICON_LEVEL }

/// `described`, in the order `ids` lists the windows, and only those `ids` lists.
fn in_listed_order(ids: &[u32], described: Vec<Listed>) -> Vec<Listed> {
    let position: std::collections::HashMap<u32, usize> =
        ids.iter().enumerate().map(|(at, id)| (*id, at)).collect();
    let mut listed: Vec<(usize, Listed)> = described
        .into_iter()
        .filter_map(|window| Some((*position.get(&window.id.as_u32())?, window)))
        .collect();
    listed.sort_by_key(|(at, _)| *at);
    listed.into_iter().map(|(_, window)| window).collect()
}

/// The ids of every window on screen or, not `on_screen`, of every window, front to back. Asked again
/// with more room while the list fills what it is given.
fn window_list(on_screen: bool) -> Vec<u32> {
    let mut room = 512usize;
    loop {
        let mut ids = vec![0u32; room];
        let (list, mut count) = (ids.as_mut_ptr(), 0 as c_int);
        let (mut set_tags, mut clear_tags) = (0u64, 0u64);
        // SAFETY: `list` heads `room` u32s the call fills at most, and the rest are locals it writes.
        let err = unsafe {
            if on_screen {
                SLSGetWindowListWithTags(
                    *READ_CONNECTION,
                    0,
                    room as c_int,
                    list,
                    &mut count,
                    1,
                    &mut set_tags,
                    &mut clear_tags,
                    64,
                )
            } else {
                SLSGetWindowList(*READ_CONNECTION, 0, room as c_int, list, &mut count)
            }
        };
        if cg_ok(err).is_err() {
            return Vec::new();
        }
        let count = count.max(0) as usize;
        if count < room || room >= 1 << 16 {
            ids.truncate(count.min(room));
            return ids;
        }
        room *= 2;
    }
}

/// `ids` described in one query, in the order given; with `titles`, each window's title as well.
fn describe(ids: &[u32], titles: bool) -> Vec<Listed> {
    if ids.is_empty() {
        return Vec::new();
    }
    let cf_numbers = cf_array_from_u64s(&ids.iter().map(|&id| id as u64).collect::<Vec<_>>());
    let Some(query) = WindowIterator::new_from_cfarray(&cf_numbers, titles as c_int) else {
        return Vec::new();
    };
    let mut described = Vec::with_capacity(ids.len());
    while query.advance().is_some() {
        described.push(Listed {
            id: WindowServerId::new(query.window_id()),
            level: query.level(),
            pid: query.pid(),
            bounds: query.bounds(),
            title: titles.then(|| query.title()).flatten(),
        });
    }
    in_listed_order(ids, described)
}

/// In a pool of its own: the threads that ask have none, and the application and its name are
/// autoreleased.
fn application_name(pid: pid_t) -> String {
    use crate::windows::platform::app::NSRunningApplicationExt;
    let named = objc2::rc::autoreleasepool(|_| {
        objc2_app_kit::NSRunningApplication::with_process_id(pid)
            .and_then(|app| app.localized_name())
            .map(|name| name.to_string())
    });
    if let Some(name) = named {
        return name;
    }
    let mut name = [0u8; 256];
    // SAFETY: the call writes at most `name.len()` bytes into `name`.
    let len = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
    String::from_utf8_lossy(&name[..len.max(0) as usize]).into_owned()
}

/// The depth of each window in `listed`, a list front to back, counting none of the desktop's.
fn depths(listed: Vec<Listed>) -> std::collections::HashMap<u32, usize> {
    listed
        .into_iter()
        .filter(|window| !is_desktop_element(window.level))
        .enumerate()
        .map(|(depth, window)| (window.id.as_u32(), depth))
        .collect()
}

fn window_is_effectively_invisible(alpha: f32, layer: i32) -> bool {
    layer == 0 && alpha <= EFFECTIVELY_INVISIBLE_WINDOW_ALPHA
}

fn window_info_from_query(query: &WindowIterator) -> Option<WindowServerInfo> {
    let layer = query.level();
    if window_is_effectively_invisible(query.alpha(), layer) {
        return None;
    }
    let (min_frame, max_frame) = query.constraints();
    Some(WindowServerInfo {
        id: WindowServerId::new(query.window_id()),
        pid: query.pid(),
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

    // SAFETY: `point` is borrowed for the call, and the rest are locals it writes.
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

pub fn current_cursor_location() -> Result<CGPoint, CGError> {
    let mut point = CGPoint::new(0.0, 0.0);
    // SAFETY: `point` is a local the call writes.
    cg_ok(unsafe { SLSGetCurrentCursorLocation(*G_CONNECTION, &mut point) })?;
    Ok(point)
}

/// Returns whether the tags describe a document or floating app window.
fn tags_match_app_window_role(tags: SLSWindowTags) -> bool {
    tags.contains(SLSWindowTags::DOCUMENT) || tags.contains(SLSWindowTags::FLOATING)
}

/// The query for the windows on `spaces` of every connection, minimized ones left out: what
/// `SLSCopyWindowsWithOptionsAndTags` answers, which waits on the main connection whatever it is
/// given.
fn space_list_filter(spaces: &[u64]) -> WindowQueryFilter<'_> {
    WindowQueryFilter {
        owner: 0,
        spaces,
        space_list_options: 0,
        window_list_options: 0x2,
        query_flags: 0,
        include_tags: 0,
        exclude_tags: 0,
    }
}

/// Whether a window the window server places on `spaces` spaces is sticky: on every space.
fn on_more_than_one_space(spaces: usize) -> bool { spaces > 1 }

/// The spaces `SLSCopySpacesForWindows` places `id` on, none when it answers nothing.
fn copy_spaces(id: WindowServerId) -> Option<CFRetained<CFArray<CFNumber>>> {
    let cf_windows = cf_array_from_ids(&[id]);
    // SAFETY: `cf_windows` is a live CFArray of CFNumbers, borrowed for the call.
    let space_list_ref = unsafe {
        SLSCopySpacesForWindows(*G_CONNECTION, 0x7, CFRetained::as_ptr(&cf_windows).as_ptr())
    };
    let space_list_ref = NonNull::new(space_list_ref)?;
    // SAFETY: a Copy call, so the array is +1 and the `CFRetained` takes that retain.
    Some(unsafe { CFRetained::from_raw(space_list_ref) })
}

impl WindowServer for LiveWindowServer {
    fn get_window(&self, id: WindowServerId) -> Option<WindowServerInfo> {
        // A single-window query rather than `get_windows(&[id])`: a query for one window handed a
        // different number back means the id names another window now, and a recycled id must
        // not be answered.
        let query = WindowIterator::new(&[id])?;
        if query.count() != 1 || query.advance().is_none() {
            return None;
        }
        window_info_from_query(&query)
    }

    fn get_windows(&self, ids: &[WindowServerId]) -> Vec<WindowServerInfo> {
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

    fn window_parent(&self, id: WindowServerId) -> Option<WindowServerId> {
        let query = WindowIterator::new(&[id])?;
        if query.count() == 1 {
            let p = query.advance()?.parent_id();
            (p != 0).then(|| WindowServerId::new(p))
        } else {
            None
        }
    }

    fn window_is_sticky(&self, id: WindowServerId) -> Option<bool> {
        Some(on_more_than_one_space(copy_spaces(id)?.len()))
    }

    fn window_level(&self, id: WindowServerId) -> Option<WindowLevel> {
        let query = WindowIterator::new(&[id])?;
        Some(query.advance()?.level() as WindowLevel)
    }

    fn window_sub_level(&self, id: WindowServerId) -> Option<c_int> {
        window_sub_level(id.as_u32())
    }

    fn window_spaces(&self, id: WindowServerId) -> Vec<SpaceId> {
        let Some(spaces) = copy_spaces(id) else {
            return Vec::new();
        };
        spaces
            .iter()
            .filter_map(|num| num.as_i64())
            .filter_map(|value| u64::try_from(value).ok())
            .filter(|&value| value != 0)
            .map(SpaceId::new)
            .collect()
    }

    fn window_ordered_in(&self, id: WindowServerId) -> Option<bool> {
        let query = WindowIterator::new(&[id])?;
        if query.advance()?.window_id() != id.as_u32() {
            return None;
        }
        Some(attributes_ordered_in(query.attributes()))
    }

    fn app_window_suitability(&self, id: WindowServerId) -> Option<bool> {
        let query = WindowIterator::new(&[id])?;
        if query.count() > 0 && query.advance().is_some() {
            Some(query.is_app_window())
        } else {
            Some(false)
        }
    }

    fn space_window_list_for_connection(&self, spaces: &[u64]) -> Vec<u32> {
        let Some(iterator) = window_query_run(&space_list_filter(spaces)) else {
            return Vec::new();
        };
        let mut windows = Vec::new();
        while iterator.advance().is_some() {
            if iterator.is_app_window() {
                windows.push(iterator.window_id());
            }
        }
        windows
    }

    fn window_under_cursor(&self) -> Option<WindowServerId> {
        let point = current_cursor_location().ok()?;
        self.get_window_at_point(point)
    }

    fn get_window_at_point(&self, mut point: CGPoint) -> Option<WindowServerId> {
        let (mut wid, cid) = find_window_at_point(&mut point, None)?;
        if is_own_window(cid) {
            wid = find_window_at_point(&mut point, Some(wid))?.0;
        }
        Some(WindowServerId(wid))
    }

    /// A parked window's written frame is there 0.04-0.1ms after the write returns, but an app still
    /// activating applies a write tens of milliseconds late ("Where a moving focus change spends its
    /// time" in `src/animation/docs/capture-overlay-research.md`).
    fn server_frame(&self, id: WindowServerId) -> Option<CGRect> {
        let mut frame = CGRect::default();
        // SAFETY: `frame` is a local the call writes.
        let read = unsafe { SLSGetWindowBounds(*READ_CONNECTION, id.as_u32(), &mut frame) };
        (read == 0 && frame.size.width > 0.0 && frame.size.height > 0.0).then_some(frame)
    }

    /// In one query, parked and off-screen windows included. Measured in
    /// `src/decorations/docs/corner.md`.
    fn corner_radii(
        &self,
        ids: &[WindowServerId],
    ) -> std::collections::HashMap<WindowServerId, f64> {
        let Some(query) = WindowIterator::new(ids) else {
            return std::collections::HashMap::new();
        };
        let mut radii = std::collections::HashMap::new();
        while query.advance().is_some() {
            if let Some(radius) = reported_radius(&query.corner_radii()) {
                radii.insert(WindowServerId::new(query.window_id()), radius);
            }
        }
        radii
    }

    /// The key-focus process can briefly differ from the frontmost one, and this does not wait on the
    /// app's `AXMainWindow`.
    fn key_focused_window(&self, space: SpaceId) -> Option<WindowId> {
        let mut psn = ProcessSerialNumber::default();
        let mut fallback = 0u8;
        // SAFETY: `psn` and `fallback` are locals the call writes.
        if cg_ok(unsafe { SLPSGetKeyFocusProcess(&mut psn, &mut fallback) }).is_err() {
            return None;
        }

        let mut owner = 0;
        // SAFETY: `psn` is borrowed for the call, and `owner` is a local it writes.
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

    fn key_focus_pid(&self) -> Option<pid_t> {
        let mut psn = ProcessSerialNumber::default();
        let mut fallback = 0u8;
        // SAFETY: `psn` and `fallback` are locals the call writes.
        cg_ok(unsafe { SLPSGetKeyFocusProcess(&mut psn, &mut fallback) }).ok()?;
        crate::windows::platform::process::pid_for_psn(&psn)
    }

    fn on_screen_windows(&self, titles: bool) -> Vec<Listed> {
        describe(&window_list(true), titles)
    }

    fn every_window(&self) -> Vec<Listed> { describe(&window_list(false), true) }

    fn owner_name(&self, pid: pid_t) -> Option<String> { Some(application_name(pid)) }

    fn desktop_windows(&self, display_uuid: &DisplayUuid) -> Vec<WindowServerId> {
        let uuid = CFString::from_str(display_uuid.as_str());
        let displays = CFArray::from_objects(&[&*uuid]);
        // SAFETY: `displays` is a live CFArray of CFStrings, borrowed for the call.
        let Some(windows) = NonNull::new(unsafe {
            SLSManagedDisplaysCopyRoleWindows(
                *G_CONNECTION,
                CFRetained::as_ptr(&displays).as_ptr(),
                1,
            )
        }) else {
            return Vec::new();
        };
        // SAFETY: a Copy call, so the array of CFNumbers is +1 and the `CFRetained` takes that retain.
        let windows: CFRetained<CFArray<CFNumber>> = unsafe { CFRetained::from_raw(windows) };
        windows
            .iter()
            .filter_map(|number| number.as_i64().and_then(|id| u32::try_from(id).ok()))
            .map(WindowServerId::new)
            .collect()
    }

    // Event-record layout, length byte 0xf8 at 0x04:
    // https://github.com/Hammerspoon/hammerspoon/issues/370#issuecomment-545545468
    fn make_key_window(&self, pid: pid_t, wsid: WindowServerId) -> Result<(), CGError> {
        #[allow(non_upper_case_globals, reason = "SkyLight's constant name")]
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

        // SAFETY: `psn` is borrowed for each call, and each event record is 0x100 bytes, more than
        // the 0xf8 its length byte at 0x04 says is read.
        unsafe {
            cg_ok(_SLPSSetFrontProcessWithOptions(&psn, wsid.0, kCPSUserGenerated))?;
            cg_ok(SLPSPostEventRecordTo(&psn, event1.as_ptr()))?;
            cg_ok(SLPSPostEventRecordTo(&psn, event2.as_ptr()))?;
        }
        Ok(())
    }

    /// The window server levels only a connection's own windows, and AppKit orders one directly above
    /// another process's window only once the two share both level and sub-level ("Ordering a rest
    /// window" in `src/decorations/docs/rest-windows.md`).
    fn take_stack_place(
        &self,
        own: WindowServerId,
        level: Option<WindowLevel>,
        sub_level: Option<c_int>,
    ) {
        for write in stack_place_writes(level, sub_level) {
            // SAFETY: integer arguments only, so no memory of rini's is at stake.
            unsafe {
                match write {
                    StackWrite::Level(level) => {
                        SLSSetWindowLevel(*G_CONNECTION, own.as_u32(), level);
                    }
                    StackWrite::SubLevel(sub_level) => {
                        SLSSetWindowSubLevel(*G_CONNECTION, own.as_u32(), sub_level);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use objc2_app_kit::NSNormalWindowLevel;

    use super::*;
    use crate::windows::domain::stack::NORMAL_WINDOW_LEVEL;

    #[test]
    fn a_window_query_key_resolves_only_a_skylight_query_key_symbol() {
        use WindowQueryKey::*;
        let names: Vec<&str> = [
            Owner,
            Spaces,
            SpaceListOptions,
            WorkspaceWindowListOptions,
            IncludeTags,
            ExcludeTags,
        ]
        .into_iter()
        .map(|key| key.symbol_name().to_str().unwrap())
        .collect();
        assert!(
            names.iter().all(|name| name.starts_with("SLSWindowQueryKey")),
            "{names:?}"
        );
        let distinct: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(distinct.len(), names.len(), "{names:?}");
    }

    #[test]
    fn zero_window_server_id_is_not_a_window_id() {
        assert!(WindowServerId::new(0).as_nonzero().is_none());
        assert_eq!(WindowServerId::new(42).as_nonzero().map(|id| id.get()), Some(42));
    }

    #[test]
    fn the_domain_s_normal_level_is_app_kit_s() {
        assert_eq!(NORMAL_WINDOW_LEVEL, NSNormalWindowLevel);
    }

    fn listed(id: u32, level: i32) -> Listed {
        Listed {
            id: WindowServerId::new(id),
            level,
            pid: 1,
            bounds: CGRect::default(),
            title: None,
        }
    }

    #[test]
    fn a_description_is_put_back_in_the_order_the_list_gave() {
        let described = vec![listed(5, 0), listed(9, 3), listed(7, 0), listed(4, 0)];
        let ordered: Vec<u32> =
            in_listed_order(&[9, 7, 5], described).iter().map(|w| w.id.as_u32()).collect();
        assert_eq!(
            ordered,
            vec![9, 7, 5],
            "front to back as listed; one never listed is dropped"
        );
    }

    #[test]
    fn a_window_is_ordered_in_by_its_second_attribute_bit_alone() {
        assert!(attributes_ordered_in(0b10));
        assert!(attributes_ordered_in(0b1110));
        assert!(!attributes_ordered_in(0b01));
        assert!(!attributes_ordered_in(0));
    }

    #[test]
    fn a_space_list_asks_for_every_owner_and_leaves_minimized_windows_out() {
        let spaces = [5];
        let shown = space_list_filter(&spaces);
        assert_eq!(
            (shown.owner, shown.spaces, shown.window_list_options),
            (0, &spaces[..], 0x2)
        );
        assert_eq!(
            (shown.include_tags, shown.exclude_tags, shown.space_list_options),
            (0, 0, 0)
        );
    }

    #[test]
    fn a_window_s_depth_counts_the_windows_in_front_of_it_but_not_the_desktop() {
        let depths = depths(vec![
            listed(30, 0),
            listed(2, -2147483626),
            listed(31, 3),
            listed(50, -2147483603),
            listed(32, 0),
        ]);
        assert_eq!(depths.get(&30), Some(&0));
        assert_eq!(
            depths.get(&31),
            Some(&1),
            "the backstop listed before it is not counted"
        );
        assert_eq!(depths.get(&32), Some(&2));
        assert_eq!(depths.get(&2), None, "a desktop element has no depth");
        assert_eq!(depths.get(&50), None);
    }

    #[test]
    fn the_desktop_is_the_icons_and_everything_below_them() {
        assert!(is_desktop_element(-2147483603), "the Finder's desktop icons");
        assert!(is_desktop_element(-2147483624), "the Dock's wallpaper");
        assert!(is_desktop_element(-2147483626), "the display's backstop");
        assert!(
            !is_desktop_element(-2147483601),
            "Notification Center's widgets are not"
        );
        assert!(!is_desktop_element(-20), "nor is a bar below the windows");
        assert!(!is_desktop_element(0));
    }

    #[test]
    fn reads_fall_back_to_the_main_connection_only_when_none_of_their_own_opens() {
        assert_eq!(opened_or(CGError::Success, 42, 7), 42);
        assert_eq!(opened_or(CGError::Failure, 42, 7), 7);
        assert_eq!(opened_or(CGError::Success, 0, 7), 7);
        assert_eq!(
            opened_or(CGError::Success, 7, 7),
            7,
            "the main connection is none of rini's own"
        );
    }
}
