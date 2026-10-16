use std::cmp::Ordering;
use std::f64;

type CGDirectDisplayID = u32;
use std::ptr::NonNull;

use objc2_app_kit::NSScreen;
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{CGDisplayBounds, CGError, CGGetActiveDisplayList, CGMainDisplayID};
use objc2_foundation::{MainThreadMarker, NSNumber, ns_string};
use rini_core::ids::{DisplayUuid, ScreenId, SpaceId};
use rini_skylight_sys::{
    CFRelease, CFUUIDCreateFromString, CFUUIDCreateString, CGDisplayCreateUUIDFromDisplayID,
    CGDisplayGetDisplayIDFromUUID, CGSCopyBestManagedDisplayForRect, CGSCopyManagedDisplaySpaces,
    CGSManagedDisplayGetCurrentSpace, CoreDockGetAutoHideEnabled, CoreDockGetOrientationAndPinning,
    G_CONNECTION, SLSCopyActiveMenuBarDisplayIdentifier, SLSGetDisplayMenubarHeight,
    SLSGetDockRectWithReason, SLSGetMenuBarAutohideEnabled, SLSGetSpaceManagementMode,
};
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, warn};

use crate::displays::domain::screen::{
    ASSUMED_SCALE, CoordinateConverter, DockEdge, ScreenInfo, menu_bar_inset, top_inset,
    usable_frame,
};

#[derive(Debug, Clone)]
struct ScreenState {
    screens: Vec<ScreenInfo>,
    converter: CoordinateConverter,
}

pub struct ScreenCache<S: System = Actual> {
    system: S,
    uuids: Vec<CFRetained<CFString>>,
    state: Option<ScreenState>,
    pending_generation: u64,
    processed_generation: u64,
    sleeping: bool,
    /// Kept clear at the top of every display over what the menu bar keeps; see `top_inset`.
    top_band: f64,
}

impl ScreenCache<Actual> {
    pub fn new(mtm: MainThreadMarker) -> Self { Self::new_with(Actual { mtm }) }
}

impl<S: System> ScreenCache<S> {
    fn new_with(system: S) -> ScreenCache<S> {
        ScreenCache {
            system,
            uuids: Vec::new(),
            state: None,
            pending_generation: 0,
            processed_generation: 0,
            sleeping: false,
            top_band: 0.0,
        }
    }

    /// Whether `band` differs from the band in force. A change rebuilds every screen at the next
    /// refresh, so the layouts move with it.
    pub fn set_top_band(&mut self, band: f64) -> bool {
        let changed = self.top_band != band;
        if changed {
            self.top_band = band;
            self.mark_dirty();
        }
        changed
    }

    pub fn mark_dirty(&mut self) {
        self.pending_generation = self.pending_generation.wrapping_add(1);
    }

    pub fn mark_sleeping(&mut self, sleeping: bool) {
        self.sleeping = sleeping;
        if !sleeping {
            self.mark_dirty();
        }
    }

    pub fn refresh(&mut self) -> Option<(Vec<ScreenInfo>, CoordinateConverter)> {
        self.refresh_snapshot().map(|s| (s.screens, s.converter))
    }

    fn refresh_snapshot(&mut self) -> Option<ScreenState> {
        if self.sleeping {
            return self.state.clone();
        }

        let dirty = self.pending_generation != self.processed_generation;
        if self.state.is_some() && !dirty {
            // Even when displays are unchanged, the active space per display can change.
            // Recompute spaces against cached UUIDs to avoid stale space ids.
            let state = self.state.clone()?;
            let screens = self.with_spaces(state.screens, &self.uuids);
            return Some(ScreenState { screens, ..state });
        }

        let ns_screens = self.system.ns_screens();
        debug!("ns_screens={ns_screens:?}");
        let mut cg_screens = self.system.cg_screens().ok()?;
        debug!("cg_screens={cg_screens:?}");

        if cg_screens.is_empty() {
            self.uuids.clear();
            let state = ScreenState {
                screens: Vec::new(),
                converter: CoordinateConverter::default(),
            };
            self.state = Some(state.clone());
            self.processed_generation = self.pending_generation;
            return Some(state);
        }

        main_first(&mut cg_screens, self.system.main_display());

        let uuids: Vec<CFRetained<CFString>> =
            cg_screens.iter().map(|screen| self.system.display_uuid(screen)).collect();
        let display_uuids: Vec<DisplayUuid> =
            uuids.iter().map(|uuid| DisplayUuid::new(uuid.to_string())).collect();

        let union_max_y = cg_screens
            .iter()
            .map(|screen| screen.bounds.max().y)
            .fold(f64::NEG_INFINITY, f64::max);
        let converter = CoordinateConverter { screen_height: union_max_y };
        let chrome = self.system.chrome();

        let screens: Vec<ScreenInfo> = cg_screens
            .iter()
            .zip(&display_uuids)
            .map(|(screen, uuid)| self.screen_info(screen, uuid, &chrome, &ns_screens))
            .collect();
        let screens = self.with_spaces(screens, &uuids);

        self.uuids = uuids;
        self.processed_generation = self.pending_generation;
        self.state = Some(ScreenState { screens, converter });
        self.state.clone()
    }

    fn screen_info(
        &self,
        &CGScreenInfo { cg_id, bounds }: &CGScreenInfo,
        uuid: &DisplayUuid,
        chrome: &Chrome,
        ns_screens: &[NSScreenInfo],
    ) -> ScreenInfo {
        let did = cg_id.as_u32();
        let menu_bar = menu_bar_inset(
            chrome.menu_bar_hidden,
            self.system.menu_bar_height(did),
            self.system.notch_height(did),
        );
        let frame = chrome.frame_on(did, bounds, top_inset(menu_bar, self.top_band));
        let display_uuid = if uuid.is_empty() {
            warn!("Missing cached UUID for {:?}; using fallback", cg_id);
            DisplayUuid::fallback(cg_id)
        } else {
            uuid.clone()
        };
        let ns_screen = ns_screens.iter().find(|s| s.cg_id == cg_id);
        ScreenInfo {
            id: cg_id,
            frame,
            bounds,
            display_uuid,
            name: ns_screen.and_then(|s| s.name.clone()),
            space: None,
            is_builtin: self.system.is_builtin(did),
            scale: ns_screen.map_or(ASSUMED_SCALE, |s| s.scale),
        }
    }

    /// `screens` each with the space its display, `uuids` in the same order, shows now.
    fn with_spaces(
        &self,
        screens: Vec<ScreenInfo>,
        uuids: &[CFRetained<CFString>],
    ) -> Vec<ScreenInfo> {
        let spaces: Vec<Option<SpaceId>> =
            uuids.iter().map(|uuid| self.system.current_space(uuid)).collect();
        screens
            .into_iter()
            .zip(spaces)
            .map(|(mut screen, space)| {
                screen.space = space;
                screen
            })
            .collect()
    }
}

/// Left to right, then top to bottom, with the main display swapped to the front.
fn main_first(cg_screens: &mut [CGScreenInfo], main_id: u32) {
    cg_screens.sort_by(|a, b| {
        let x_order = a.bounds.origin.x.total_cmp(&b.bounds.origin.x);
        if x_order == Ordering::Equal {
            a.bounds.origin.y.total_cmp(&b.bounds.origin.y)
        } else {
            x_order
        }
    });
    if let Some(main_screen_idx) = cg_screens.iter().position(|s| s.cg_id.as_u32() == main_id) {
        cg_screens.swap(0, main_screen_idx);
    } else {
        warn!("Could not find main screen. cg_screens={cg_screens:?}");
    }
}

const DOCK_ORIENTATION_LEFT: i32 = 1;
const DOCK_ORIENTATION_BOTTOM: i32 = 2;
const DOCK_ORIENTATION_RIGHT: i32 = 3;

/// The menu bar and the Dock as they cover the displays, read once a rebuild.
#[derive(Debug, Clone, Copy)]
struct Chrome {
    menu_bar_hidden: bool,
    /// The Dock while it is shown, which an auto-hidden one is only while it is out.
    dock: Option<Dock>,
}

#[derive(Debug, Clone, Copy)]
struct Dock {
    rect: CGRect,
    edge: DockEdge,
    /// The display it is on, `None` when SkyLight names none.
    display: Option<u32>,
}

impl Chrome {
    /// What of `raw`, display `did`'s bounds, is left once `top` is kept clear and a Dock on `did`
    /// is taken out. A Dock on another display covers nothing here.
    fn frame_on(&self, did: u32, raw: CGRect, top: f64) -> CGRect {
        let dock = self.dock.filter(|dock| dock.display == Some(did));
        usable_frame(raw, top, dock.map(|dock| (dock.rect, dock.edge)))
    }
}

fn dock_edge(orientation: i32) -> DockEdge {
    match orientation {
        DOCK_ORIENTATION_LEFT => DockEdge::Left,
        DOCK_ORIENTATION_RIGHT => DockEdge::Right,
        DOCK_ORIENTATION_BOTTOM => DockEdge::Bottom,
        _ => DockEdge::Unknown,
    }
}

fn menu_bar_hidden() -> bool {
    let mut status = 0;
    // SAFETY: `status` is a local the call writes.
    unsafe { SLSGetMenuBarAutohideEnabled(*G_CONNECTION, &mut status) };
    status != 0
}

fn dock_hidden() -> bool {
    // SAFETY: takes nothing and answers a bool.
    unsafe { CoreDockGetAutoHideEnabled() }
}

fn dock_orientation() -> i32 {
    let mut orientation = 0;
    let mut pinning = 0;
    // SAFETY: `orientation` and `pinning` are locals the call writes.
    unsafe { CoreDockGetOrientationAndPinning(&mut orientation, &mut pinning) };
    orientation
}

fn dock_rect_with_reason() -> (CGRect, i32) {
    let mut rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0));
    let mut reason = 0;
    // SAFETY: `rect` and `reason` are locals the call writes.
    unsafe { SLSGetDockRectWithReason(*G_CONNECTION, &mut rect, &mut reason) };
    (rect, reason)
}

/// The uuid of the display that best holds `rect`, as `connection` sees it.
fn best_display_for_rect(connection: i32, rect: CGRect) -> Option<CFRetained<CFString>> {
    // SAFETY: a rect by value; the uuid comes back +1 under the Copy rule, or null.
    let uuid = NonNull::new(unsafe { CGSCopyBestManagedDisplayForRect(connection, rect) })?;
    // SAFETY: the +1 just made, which the `CFRetained` takes.
    Some(unsafe { CFRetained::from_raw(uuid) })
}

fn display_of_rect(rect: CGRect) -> Option<u32> {
    let uuid = best_display_for_rect(*G_CONNECTION, rect)?;
    display_id_of_uuid(&uuid)
}

/// Whether the display `did` is the built-in one.
fn display_is_builtin(did: u32) -> bool {
    // SAFETY: an integer argument only.
    unsafe { rini_skylight_sys::CGDisplayIsBuiltin(did) }
}

/// The id of the space the display with `uuid` shows, 0 for none.
fn current_space_id(uuid: &CFString) -> u64 {
    // SAFETY: `uuid` is a live CFString, borrowed for the call, which only reads it.
    unsafe { CGSManagedDisplayGetCurrentSpace(*G_CONNECTION, uuid as *const CFString as *mut _) }
}

/// The CoreGraphics id of the display with this uuid, `None` when no display has it.
fn display_id_of_uuid(uuid: &CFString) -> Option<u32> {
    // SAFETY: `uuid` is live for the call; the CFUUID made from it comes back +1 under the Create
    // rule, read once and released here.
    unsafe {
        let parsed =
            CFUUIDCreateFromString(std::ptr::null_mut(), uuid as *const CFString as *mut _);
        if parsed.is_null() {
            return None;
        }
        let did = CGDisplayGetDisplayIDFromUUID(parsed);
        CFRelease(parsed);
        (did != 0).then_some(did)
    }
}

#[allow(private_interfaces, reason = "ScreenCache's test seam")]
pub trait System {
    fn cg_screens(&self) -> Result<Vec<CGScreenInfo>, CGError>;
    fn display_uuid(&self, screen: &CGScreenInfo) -> CFRetained<CFString>;
    fn ns_screens(&self) -> Vec<NSScreenInfo>;
    fn notch_height(&self, did: u32) -> f64;
    fn main_display(&self) -> u32;
    fn is_builtin(&self, did: u32) -> bool;
    /// The space the display with `uuid` shows, `None` for no answer.
    fn current_space(&self, uuid: &CFString) -> Option<SpaceId>;
    fn chrome(&self) -> Chrome;
    fn menu_bar_height(&self, did: u32) -> f64;
}

#[derive(Debug, Clone)]
struct CGScreenInfo {
    cg_id: ScreenId,
    bounds: CGRect,
}

#[derive(Debug, Clone)]
struct NSScreenInfo {
    cg_id: ScreenId,
    name: Option<String>,
    scale: f64,
}

pub struct Actual {
    mtm: MainThreadMarker,
}
#[allow(private_interfaces, reason = "ScreenCache's test seam")]
impl System for Actual {
    fn cg_screens(&self) -> Result<Vec<CGScreenInfo>, CGError> {
        const MAX_SCREENS: usize = 64;
        let mut ids = [0 as CGDirectDisplayID; MAX_SCREENS];
        let mut count: u32 = 0;
        // SAFETY: `ids` holds the `MAX_SCREENS` ids the call writes at most, and `count` is a local
        // it writes.
        let err =
            unsafe { CGGetActiveDisplayList(MAX_SCREENS as u32, ids.as_mut_ptr(), &mut count) };
        if err != CGError::Success {
            return Err(err);
        }
        Ok(ids[..(count as usize).min(MAX_SCREENS)]
            .iter()
            .map(|&cg_id| CGScreenInfo {
                cg_id: ScreenId::new(cg_id),
                bounds: CGDisplayBounds(cg_id),
            })
            .collect())
    }

    fn display_uuid(&self, screen: &CGScreenInfo) -> CFRetained<CFString> {
        // SAFETY: an integer argument only; the CFUUID comes back +1 under the Create rule, or null.
        let uuid = NonNull::new(unsafe { CGDisplayCreateUUIDFromDisplayID(screen.cg_id.as_u32()) });
        if let Some(uuid) = uuid {
            // SAFETY: `uuid` is the live CFUUID just made, and the default allocator is asked for.
            let uuid_str = unsafe { CFUUIDCreateString(std::ptr::null_mut(), uuid.as_ptr()) };
            // SAFETY: the +1 made above, not used again.
            unsafe { CFRelease(uuid.as_ptr()) };
            if let Some(uuid_str) = NonNull::new(uuid_str) {
                // SAFETY: a Create call's +1, which the `CFRetained` takes.
                return unsafe { CFRetained::from_raw(uuid_str) };
            } else {
                warn!(
                    "CGDisplayCreateUUIDFromDisplayID returned invalid string for {:?}",
                    screen
                );
            }
        } else {
            warn!(
                "CGDisplayCreateUUIDFromDisplayID returned null for display {:?}",
                screen.cg_id
            );
        }
        if let Some(managed) = best_display_for_rect(*G_CONNECTION, screen.bounds) {
            managed
        } else {
            warn!(
                "CGSCopyBestManagedDisplayForRect returned null for display {:?}",
                screen.cg_id
            );
            CFString::from_str("")
        }
    }

    fn ns_screens(&self) -> Vec<NSScreenInfo> {
        NSScreen::screens(self.mtm)
            .iter()
            .flat_map(|s| {
                let name = s.localizedName().to_string();
                Some(NSScreenInfo {
                    cg_id: s.get_number()?,
                    name: Some(name),
                    scale: s.backingScaleFactor(),
                })
            })
            .collect()
    }

    fn notch_height(&self, did: u32) -> f64 {
        let screens = NSScreen::screens(self.mtm);
        if !display_is_builtin(did) {
            return 0.0;
        }

        for screen in screens {
            if let Some(screen_id) = screen.get_number()
                && screen_id.as_u32() == did
            {
                let insets = screen.safeAreaInsets();
                return insets.top;
            }
        }
        0.0
    }

    fn main_display(&self) -> u32 { CGMainDisplayID() }

    fn is_builtin(&self, did: u32) -> bool { display_is_builtin(did) }

    fn current_space(&self, uuid: &CFString) -> Option<SpaceId> {
        let id = current_space_id(uuid);
        (id != 0).then(|| SpaceId::new(id))
    }

    fn chrome(&self) -> Chrome {
        let (rect, reason) = dock_rect_with_reason();
        let shown = !dock_hidden() || reason == 0;
        Chrome {
            menu_bar_hidden: menu_bar_hidden(),
            dock: shown.then(|| Dock {
                rect,
                edge: dock_edge(dock_orientation()),
                display: display_of_rect(rect),
            }),
        }
    }

    fn menu_bar_height(&self, did: u32) -> f64 {
        let mut height: u32 = 0;
        // SAFETY: `height` is a local the call writes.
        unsafe { SLSGetDisplayMenubarHeight(did, &mut height) };
        height as f64
    }
}

pub trait NSScreenExt {
    fn get_number(&self) -> Option<ScreenId>;
}
impl NSScreenExt for NSScreen {
    fn get_number(&self) -> Option<ScreenId> {
        let desc = self.deviceDescription();
        let val = desc.objectForKey(ns_string!("NSScreenNumber"));
        if let Some(number) = val.as_deref().and_then(|val| val.downcast_ref::<NSNumber>()) {
            return Some(ScreenId::new(number.as_u32()));
        }
        warn!(
            "Could not get NSScreenNumber for screen with name {:?}: {:?}",
            self.localizedName(),
            val,
        );
        None
    }
}

/// The primary display's bottom edge, which AppKit's bottom-left coordinates are anchored to: the
/// height a `CoordinateConverter` needs to place a window.
pub fn primary_display_height() -> f64 {
    let bounds = CGDisplayBounds(CGMainDisplayID());
    bounds.origin.y + bounds.size.height
}

pub fn get_active_space_number() -> Option<SpaceId> {
    active_menu_bar_display_uuid().and_then(|uuid| current_space_for_display_uuid(&uuid))
}

pub fn active_menu_bar_display_uuid() -> Option<DisplayUuid> {
    active_menu_bar_display().map(|uuid| DisplayUuid::new(uuid.to_string()))
}

/// The CoreGraphics id of the display whose menu bar is active.
pub fn active_menu_bar_display_id() -> Option<u32> {
    active_menu_bar_display().and_then(|uuid| display_id_of_uuid(&uuid))
}

fn active_menu_bar_display() -> Option<CFRetained<CFString>> {
    // SAFETY: an integer argument only; the identifier comes back +1 under the Copy rule, or null.
    let uuid = NonNull::new(unsafe { SLSCopyActiveMenuBarDisplayIdentifier(*G_CONNECTION) })?;
    // SAFETY: the +1 just made, which the `CFRetained` takes.
    Some(unsafe { CFRetained::from_raw(uuid) })
}

pub fn current_space_for_display_uuid(display_uuid: &DisplayUuid) -> Option<SpaceId> {
    if display_uuid.is_empty() {
        return None;
    }

    let id = current_space_id(&CFString::from_str(display_uuid.as_str()));
    (id != 0).then(|| SpaceId::new(id))
}

pub fn displays_have_separate_spaces() -> bool {
    // SAFETY: an integer argument only.
    unsafe { SLSGetSpaceManagementMode(*G_CONNECTION) == 1 }
}

pub fn managed_display_space_ids() -> HashMap<DisplayUuid, Vec<SpaceId>> {
    let mut out: HashMap<DisplayUuid, Vec<SpaceId>> = HashMap::default();
    // SAFETY: an integer argument only; the array comes back +1 under the Copy rule, or null.
    let Some(raw) = NonNull::new(unsafe { CGSCopyManagedDisplaySpaces(*G_CONNECTION) }) else {
        return out;
    };
    // SAFETY: the +1 just made, which the `CFRetained` takes; an NSArray is toll-free a CFArray,
    // whose entries are CF objects, each downcast before it is read.
    let displays: CFRetained<CFArray<CFType>> = unsafe { CFRetained::from_raw(raw.cast()) };
    let display_id_key = CFString::from_static_str("Display Identifier");
    let spaces_key = CFString::from_static_str("Spaces");
    let managed_space_id_key = CFString::from_static_str("ManagedSpaceID");

    for display_entry in displays.iter() {
        let Some(display_entry) = string_keyed(display_entry) else {
            continue;
        };
        let display_id = display_entry
            .get(&display_id_key)
            .and_then(|v| v.downcast::<CFString>().ok())
            .map(|v| v.to_string())
            .unwrap_or_default();
        if display_id.is_empty() {
            continue;
        }

        let mut space_ids = Vec::new();
        if let Some(spaces_value) = display_entry.get(&spaces_key)
            && let Ok(spaces) = spaces_value.downcast::<CFArray>()
        {
            // SAFETY: every entry of a CFArray is a CF object, and each is downcast before it is
            // read.
            let spaces = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(spaces) };
            for space_entry in spaces.iter() {
                if let Some(space_dict) = string_keyed(space_entry) {
                    let space_id = space_dict
                        .get(&managed_space_id_key)
                        .and_then(|v| v.downcast::<CFNumber>().ok())
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    if space_id != 0 {
                        space_ids.push(SpaceId::new(space_id as u64));
                    }
                }
            }
        }

        out.insert(DisplayUuid::new(display_id), space_ids);
    }

    out
}

/// `value` as a dictionary looked up by string, none when it is no dictionary.
fn string_keyed(value: CFRetained<CFType>) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
    let dictionary = value.downcast::<CFDictionary>().ok()?;
    // SAFETY: a lookup by a CFString key compares it with CFEqual, sound whatever the dictionary's
    // keys are, and every value is a CF object, each downcast before it is read.
    Some(unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(dictionary) })
}

#[cfg(test)]
mod test {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use objc2_core_foundation::{CFRetained, CFString, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::CGError;

    use super::{
        CGScreenInfo, Chrome, DisplayUuid, Dock, DockEdge, NSScreenInfo, ScreenCache, ScreenId,
        SpaceId, System, main_first,
    };

    /// What a stub machine answers beside its screens: the menu bar hidden, no Dock shown, no
    /// display main or built in and no space known, until a test says otherwise.
    struct Machine {
        main: u32,
        builtin: Vec<u32>,
        space: Cell<Option<SpaceId>>,
        chrome: Chrome,
        menu_bar_height: f64,
    }

    impl Default for Machine {
        fn default() -> Self {
            Machine {
                main: 0,
                builtin: Vec::new(),
                space: Cell::new(None),
                chrome: Chrome {
                    menu_bar_hidden: true,
                    dock: None,
                },
                menu_bar_height: 0.0,
            }
        }
    }

    struct Stub {
        cg_screens: Vec<CGScreenInfo>,
        ns_screens: Vec<NSScreenInfo>,
        machine: Machine,
    }
    impl System for Stub {
        fn cg_screens(&self) -> Result<Vec<CGScreenInfo>, CGError> { Ok(self.cg_screens.clone()) }

        fn display_uuid(&self, _screen: &CGScreenInfo) -> CFRetained<CFString> {
            CFString::from_str("stub")
        }

        fn ns_screens(&self) -> Vec<NSScreenInfo> { self.ns_screens.clone() }

        fn notch_height(&self, _did: u32) -> f64 { 0.0 }

        fn main_display(&self) -> u32 { self.machine.main }

        fn is_builtin(&self, did: u32) -> bool { self.machine.builtin.contains(&did) }

        fn current_space(&self, _uuid: &CFString) -> Option<SpaceId> { self.machine.space.get() }

        fn chrome(&self) -> Chrome { self.machine.chrome }

        fn menu_bar_height(&self, _did: u32) -> f64 { self.machine.menu_bar_height }
    }

    struct SequenceSystem {
        cg_screens: RefCell<VecDeque<Vec<CGScreenInfo>>>,
        ns_screens: RefCell<VecDeque<Vec<NSScreenInfo>>>,
        uuids: RefCell<VecDeque<CFRetained<CFString>>>,
        machine: Machine,
    }

    impl SequenceSystem {
        fn new(
            cg_screens: Vec<Vec<CGScreenInfo>>,
            ns_screens: Vec<Vec<NSScreenInfo>>,
            uuids: Vec<CFRetained<CFString>>,
        ) -> Self {
            Self {
                cg_screens: RefCell::new(VecDeque::from(cg_screens)),
                ns_screens: RefCell::new(VecDeque::from(ns_screens)),
                uuids: RefCell::new(VecDeque::from(uuids)),
                machine: Machine::default(),
            }
        }
    }

    impl System for SequenceSystem {
        fn cg_screens(&self) -> Result<Vec<CGScreenInfo>, CGError> {
            Ok(self.cg_screens.borrow_mut().pop_front().unwrap_or_default())
        }

        fn display_uuid(&self, _screen: &CGScreenInfo) -> CFRetained<CFString> {
            self.uuids
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| CFString::from_str("missing-uuid"))
        }

        fn ns_screens(&self) -> Vec<NSScreenInfo> {
            self.ns_screens.borrow_mut().pop_front().unwrap_or_default()
        }

        fn notch_height(&self, _did: u32) -> f64 { 0.0 }

        fn main_display(&self) -> u32 { self.machine.main }

        fn is_builtin(&self, did: u32) -> bool { self.machine.builtin.contains(&did) }

        fn current_space(&self, _uuid: &CFString) -> Option<SpaceId> { self.machine.space.get() }

        fn chrome(&self) -> Chrome { self.machine.chrome }

        fn menu_bar_height(&self, _did: u32) -> f64 { self.machine.menu_bar_height }
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    }

    fn cg(id: u32, bounds: CGRect) -> CGScreenInfo {
        CGScreenInfo {
            cg_id: ScreenId::new(id),
            bounds,
        }
    }

    fn ns(id: u32, scale: f64) -> NSScreenInfo {
        NSScreenInfo {
            cg_id: ScreenId::new(id),
            name: None,
            scale,
        }
    }

    /// The menu bar keeps its height and a pixel clear on every display; the Dock is taken out of
    /// the display it is on alone.
    #[test]
    fn a_screen_s_frame_is_its_bounds_less_the_menu_bar_and_a_dock_on_its_own_display() {
        let stub = Stub {
            cg_screens: vec![
                cg(1, rect(3840.0, 1080.0, 1512.0, 982.0)),
                cg(3, rect(0.0, 0.0, 3840.0, 2160.0)),
            ],
            ns_screens: vec![ns(3, 2.0), ns(1, 2.0)],
            machine: Machine {
                main: 3,
                chrome: Chrome {
                    menu_bar_hidden: false,
                    dock: Some(Dock {
                        rect: rect(0.0, 2080.0, 3840.0, 80.0),
                        edge: DockEdge::Bottom,
                        display: Some(3),
                    }),
                },
                menu_bar_height: 24.0,
                ..Machine::default()
            },
        };
        let (screens, _) = ScreenCache::new_with(stub).refresh().unwrap();
        let frame_of = |id: u32| screens.iter().find(|s| s.id == ScreenId::new(id)).unwrap().frame;

        assert_eq!(frame_of(1), rect(3840.0, 1105.0, 1512.0, 957.0));
        assert_eq!(frame_of(3), rect(0.0, 25.0, 3840.0, 2055.0));
    }

    #[test]
    fn the_main_display_comes_first() {
        let stub = Stub {
            cg_screens: vec![
                cg(1, rect(0.0, 0.0, 1440.0, 900.0)),
                cg(2, rect(1440.0, 0.0, 1440.0, 900.0)),
            ],
            ns_screens: vec![],
            machine: Machine { main: 2, ..Machine::default() },
        };
        let (screens, _) = ScreenCache::new_with(stub).refresh().unwrap();
        assert_eq!(screens.iter().map(|s| s.id).collect::<Vec<_>>(), [
            ScreenId::new(2),
            ScreenId::new(1)
        ]);
    }

    /// Sorted left to right and then top to bottom, the main display swapped to the front; with
    /// no main among them, sorted alone.
    #[test]
    fn displays_run_left_to_right_then_top_to_bottom_with_the_main_one_swapped_first() {
        let ids =
            |screens: &[CGScreenInfo]| screens.iter().map(|s| s.cg_id.as_u32()).collect::<Vec<_>>();
        let unsorted = vec![
            cg(1, rect(1440.0, 0.0, 1440.0, 900.0)),
            cg(2, rect(0.0, 900.0, 1440.0, 900.0)),
            cg(3, rect(0.0, 0.0, 1440.0, 900.0)),
        ];
        let mut screens = unsorted.clone();
        main_first(&mut screens, 1);
        assert_eq!(ids(&screens), [1, 2, 3]);

        let mut screens = unsorted;
        main_first(&mut screens, 9);
        assert_eq!(ids(&screens), [3, 2, 1]);
    }

    /// A refresh with nothing dirty rebuilds no screen but still asks which space each shows.
    #[test]
    fn each_screen_carries_the_space_its_display_shows_now_and_whether_it_is_built_in() {
        let stub = Stub {
            cg_screens: vec![
                cg(1, rect(0.0, 0.0, 1440.0, 900.0)),
                cg(2, rect(1440.0, 0.0, 1440.0, 900.0)),
            ],
            ns_screens: vec![],
            machine: Machine {
                builtin: vec![2],
                space: Cell::new(Some(SpaceId::new(7))),
                ..Machine::default()
            },
        };
        let mut cache = ScreenCache::new_with(stub);
        let (screens, _) = cache.refresh().unwrap();
        assert!(screens.iter().all(|s| s.space == Some(SpaceId::new(7))));
        let builtin: Vec<_> = screens.iter().filter(|s| s.is_builtin).map(|s| s.id).collect();
        assert_eq!(builtin, [ScreenId::new(2)]);

        cache.system.machine.space.set(Some(SpaceId::new(9)));
        let (screens, _) = cache.refresh().unwrap();
        assert!(screens.iter().all(|s| s.space == Some(SpaceId::new(9))));
    }

    /// A Retina built-in beside a 1x external: each screen is measured in its own pixels, which is
    /// what a capture read off its framebuffer comes back in.
    #[test]
    fn each_screen_carries_the_scale_its_display_reports() {
        let stub = Stub {
            cg_screens: vec![
                cg(1, rect(0.0, 0.0, 1440.0, 900.0)),
                cg(2, rect(1440.0, 0.0, 1440.0, 900.0)),
            ],
            ns_screens: vec![ns(1, 2.0), ns(2, 1.0)],
            machine: Machine::default(),
        };
        let (screens, _) = ScreenCache::new_with(stub).refresh().unwrap();
        let scale_of = |id: u32| screens.iter().find(|s| s.id == ScreenId::new(id)).unwrap().scale;
        assert_eq!(scale_of(1), 2.0);
        assert_eq!(scale_of(2), 1.0);
    }

    #[test]
    fn clears_cached_screen_identifiers_when_display_list_is_empty() {
        let system = SequenceSystem::new(
            vec![vec![cg(1, rect(0.0, 0.0, 1440.0, 900.0))], vec![]],
            vec![vec![ns(1, 2.0)], vec![]],
            vec![CFString::from_str("uuid-1")],
        );

        let mut cache = ScreenCache::new_with(system);

        let (screens, _) = cache.refresh().unwrap();
        assert_eq!(screens.len(), 1);
        assert_eq!(cache.uuids.len(), 1);

        cache.mark_dirty();
        let (screens, converter) = cache.refresh().unwrap();
        assert!(screens.is_empty());
        assert!(cache.uuids.is_empty());
        assert!(converter.convert_point(CGPoint::new(0.0, 0.0)).is_none());
    }

    #[test]
    fn a_display_with_no_uuid_is_named_by_its_fallback() {
        let system = SequenceSystem::new(
            vec![vec![cg(7, rect(0.0, 0.0, 1440.0, 900.0))]],
            vec![vec![ns(7, 2.0)]],
            vec![CFString::from_str("")],
        );

        let (screens, _) = ScreenCache::new_with(system).refresh().unwrap();

        let fallback = DisplayUuid::fallback(ScreenId::new(7));
        assert_eq!(screens[0].display_uuid, fallback);
        assert_eq!(screens[0].display_uuid_opt(), Some(&fallback));
    }

    /// The external keeps nothing clear with the menu bar hidden, so a band reserved for a bar has to
    /// reach it through the cache, and a change has to rebuild what the cache already holds.
    #[test]
    fn a_top_band_rebuilds_the_screens_and_keeps_its_height_clear() {
        let screen = cg(1, rect(0.0, 0.0, 1440.0, 900.0));
        let system = SequenceSystem::new(
            vec![vec![screen.clone()], vec![screen]],
            vec![vec![ns(1, 2.0)], vec![ns(1, 2.0)]],
            vec![CFString::from_str("uuid-1"), CFString::from_str("uuid-1")],
        );
        let mut cache = ScreenCache::new_with(system);
        cache.refresh().unwrap();

        assert!(cache.set_top_band(32.0));
        let (screens, _) = cache.refresh().unwrap();
        assert!(
            cache.system.cg_screens.borrow().is_empty(),
            "the band change rebuilt the screens"
        );
        assert_eq!(screens[0].frame, rect(0.0, 32.0, 1440.0, 868.0));

        assert!(!cache.set_top_band(32.0));
        let (screens, _) = cache.refresh().unwrap();
        assert_eq!(screens.len(), 1, "the same band again rebuilt nothing");
    }
}
