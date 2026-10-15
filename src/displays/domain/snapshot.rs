//! How the spaces actor builds the snapshot it forwards, from the screens and the window server's
//! answers it read: which space each screen may show, what changed since the snapshot before, which
//! windows sit on which active space, and which windows a display reconfiguration moved.
use objc2_core_foundation::CGSize;
use rini_core::ids::SpaceId;
use rini_skylight_sys::{DisplayReconfigFlags, WindowServerId};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use crate::displays::domain::screen::ScreenInfo;
use crate::displays::domain::topology::{
    ForwardedSpaceState, SpaceEventKind, Topology, TopologyWindowDelta,
};

/// How a space id is classified.
///
/// Two window-server calls per space, injected rather than called directly, because every rule
/// that decides what to forward asks one of them.
#[derive(Clone, Copy)]
pub struct SpaceKinds {
    /// A fullscreen space, which is transient native state and must not reach the reactor.
    pub(crate) is_fullscreen: fn(SpaceId) -> bool,
    /// A user space (`SLSSpaceGetType == 0`). A login or system space is neither.
    pub(crate) is_user: fn(SpaceId) -> bool,
}

impl SpaceKinds {
    /// The classification tests get by default: ids at or above `TEST_FULLSCREEN_SPACE` are
    /// fullscreen, and everything else is a user space. A test that needs a non-user space builds
    /// its own `SpaceKinds`.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            is_fullscreen: |space| space.get() >= TEST_FULLSCREEN_SPACE,
            is_user: |_| true,
        }
    }

    pub(crate) fn is_fullscreen(&self, space: SpaceId) -> bool { (self.is_fullscreen)(space) }

    pub(crate) fn is_user(&self, space: SpaceId) -> bool { (self.is_user)(space) }

    pub(crate) fn classify(&self, space: SpaceId) -> Option<SpaceEventKind> {
        if self.is_fullscreen(space) {
            Some(SpaceEventKind::Fullscreen)
        } else {
            self.is_user(space).then_some(SpaceEventKind::User)
        }
    }
}

/// The lowest space id the test classifier treats as fullscreen. macOS fullscreen space ids are far
/// above any user space id, which is what makes a threshold a usable stand-in.
#[cfg(test)]
pub(crate) const TEST_FULLSCREEN_SPACE: u64 = 0x400000000;

/// What the spaces actor keeps of the snapshots it forwarded, which the next one is measured
/// against.
#[derive(Debug, Default)]
pub struct Forwarded {
    pub screens: Vec<ScreenInfo>,
    pub has_seen_display_set: bool,
    /// The user space each display last showed, by display UUID.
    pub last_user_space_by_display: HashMap<String, SpaceId>,
    pub active_display_uuid: Option<String>,
    /// Which active space each window the window server listed is on.
    pub visible_window_spaces: HashMap<WindowServerId, SpaceId>,
}

/// What the window server answered while a snapshot was taken.
#[derive(Debug, Default)]
pub struct Heard {
    /// The display whose menu bar is active.
    pub active_display: Option<String>,
    /// The space showing on that display.
    pub active_space: Option<SpaceId>,
    /// Every managed space, per display UUID; empty when the window server did not answer.
    pub spaces_per_display: HashMap<String, Vec<SpaceId>>,
}

/// One snapshot to forward: its screens as `user_screens` left them, and what came with them.
#[derive(Debug, Default)]
pub struct Taken {
    pub screens: Vec<ScreenInfo>,
    /// The fullscreen spaces the screens showed before `user_screens` nulled them out.
    pub fullscreen_spaces: HashSet<SpaceId>,
    pub heard: Heard,
    /// `visible_window_spaces` over the screens.
    pub windows: HashMap<WindowServerId, SpaceId>,
    pub releases_lifecycle_refresh_quarantine: bool,
    pub topology_window_delta: Option<TopologyWindowDelta>,
}

/// `screens` with each space rini does not manage nulled out, and the fullscreen spaces they
/// showed. A display entering fullscreen while another display takes over its user space keeps the
/// user space it had (`keep_user_spaces_through_fullscreen`).
pub fn user_screens(
    previous: &[ScreenInfo],
    mut screens: Vec<ScreenInfo>,
    kinds: SpaceKinds,
) -> (Vec<ScreenInfo>, HashSet<SpaceId>) {
    let fullscreen_spaces = screens
        .iter()
        .filter_map(|screen| screen.space)
        .filter(|space| kinds.is_fullscreen(*space))
        .collect();
    keep_user_spaces_through_fullscreen(previous, &mut screens, kinds);
    for screen in &mut screens {
        if screen
            .space
            .is_some_and(|space| kinds.is_fullscreen(space) || !kinds.is_user(space))
        {
            screen.space = None;
        }
    }
    (screens, fullscreen_spaces)
}

/// While one display enters fullscreen, macOS can report another display showing the user space
/// the first one showed. That display keeps its own previous user space instead.
fn keep_user_spaces_through_fullscreen(
    previous: &[ScreenInfo],
    screens: &mut [ScreenInfo],
    kinds: SpaceKinds,
) {
    let previous_by_display: HashMap<&str, &ScreenInfo> = previous
        .iter()
        .filter_map(|screen| screen.display_uuid_opt().map(|uuid| (uuid, screen)))
        .collect();
    let previous_space = |display: &str| previous_by_display.get(display).and_then(|s| s.space);
    let entering_fullscreen = screens.iter().any(|next| {
        next.display_uuid_opt().zip(next.space).is_some_and(|(display, space)| {
            kinds.is_fullscreen(space)
                && previous_space(display).is_some_and(|before| !kinds.is_fullscreen(before))
        })
    });
    if !entering_fullscreen {
        return;
    }
    let previous_owner: HashMap<SpaceId, &str> = previous
        .iter()
        .filter_map(|screen| Some((screen.space?, screen.display_uuid_opt()?)))
        .filter(|(space, _)| !kinds.is_fullscreen(*space))
        .collect();
    for next in screens.iter_mut() {
        let Some(display) = next.display_uuid_opt() else {
            continue;
        };
        let (Some(space), Some(before)) = (next.space, previous_space(display)) else {
            continue;
        };
        if kinds.is_fullscreen(space) || before == space || kinds.is_fullscreen(before) {
            continue;
        }
        if previous_owner.get(&space).is_some_and(|owner| *owner != display) {
            next.space = Some(before);
        }
    }
}

/// The snapshot to forward, measured against what `memory` kept of the last one, which it then
/// takes the place of.
pub fn forward(memory: &mut Forwarded, taken: Taken) -> ForwardedSpaceState {
    let Taken { screens, heard, .. } = &taken;
    let previous = &memory.screens;
    let topology = Topology::between(previous, screens, memory.has_seen_display_set);
    let resized_spaces = resized_spaces(previous, screens);
    let space_remaps = space_remaps(
        &mut memory.last_user_space_by_display,
        screens,
        topology.allows_space_remap(),
    );
    let menu_bar_space = menu_bar_space(screens, heard.active_space, previous);
    let command_space = command_space(
        screens,
        heard.active_display.as_deref(),
        heard.active_space,
        previous,
    );
    memory.active_display_uuid = heard
        .active_display
        .clone()
        .filter(|uuid| screens.iter().any(|screen| screen.display_uuid == *uuid))
        .or_else(|| {
            let space = command_space?;
            let screen = screens.iter().find(|screen| screen.space == Some(space))?;
            Some(screen.display_uuid.clone())
        });
    let Taken {
        screens,
        fullscreen_spaces,
        heard,
        windows,
        ..
    } = taken;
    let (display_space_ids, display_space_ids_from_screens) =
        spaces_per_display(heard.spaces_per_display, &screens);
    memory.has_seen_display_set |= !screens.is_empty();
    memory.visible_window_spaces = windows;
    memory.screens = screens.clone();
    ForwardedSpaceState {
        active_spaces: screens.iter().filter_map(|screen| screen.space).collect(),
        screens,
        fullscreen_spaces,
        has_seen_display_set: memory.has_seen_display_set,
        menu_bar_space,
        command_space,
        display_space_ids,
        display_space_ids_from_screens,
        last_user_space_by_display: memory.last_user_space_by_display.clone(),
        space_remaps,
        topology,
        releases_lifecycle_refresh_quarantine: taken.releases_lifecycle_refresh_quarantine,
        resized_spaces,
        topology_window_delta: taken.topology_window_delta,
        active_window_spaces: memory.visible_window_spaces.clone(),
    }
}

/// The space and new size of each screen that is new or whose size changed by a whole point.
fn resized_spaces(previous: &[ScreenInfo], screens: &[ScreenInfo]) -> Vec<(SpaceId, CGSize)> {
    let previous_sizes: HashMap<_, _> =
        previous.iter().map(|screen| (screen.id, screen.frame.size)).collect();
    let rounded = |size: CGSize| (size.width.round() as i32, size.height.round() as i32);
    screens
        .iter()
        .filter(|screen| {
            previous_sizes
                .get(&screen.id)
                .is_none_or(|before| rounded(*before) != rounded(screen.frame.size))
        })
        .filter_map(|screen| screen.space.map(|space| (space, screen.frame.size)))
        .collect()
}

/// Each display's move from the user space it last showed to the one it shows now, when remaps are
/// allowed, with `history` brought up to date either way.
///
/// A display showing a space `history` gives another display is skipped, history and all: during
/// sleep and wake macOS can briefly report the remaining display with the space of one that has not
/// reappeared, and accepting it remaps that display's layout onto the wrong one.
fn space_remaps(
    history: &mut HashMap<String, SpaceId>,
    screens: &[ScreenInfo],
    allow_space_remap: bool,
) -> Vec<(SpaceId, SpaceId)> {
    let current_owners: HashMap<SpaceId, &str> = screens
        .iter()
        .filter_map(|screen| Some((screen.space?, screen.display_uuid_opt()?)))
        .collect();
    let historical_owners: HashMap<SpaceId, String> =
        history.iter().map(|(display, &space)| (space, display.clone())).collect();
    let mut seen_displays: HashSet<&str> = HashSet::default();
    let mut remaps = Vec::new();
    for screen in screens {
        let (Some(space), Some(display)) = (screen.space, screen.display_uuid_opt()) else {
            continue;
        };
        if !seen_displays.insert(display)
            || historical_owners.get(&space).is_some_and(|owner| owner != display)
        {
            continue;
        }
        if let Some(before) = history.get(display).copied()
            && before != space
            && allow_space_remap
            && current_owners.get(&before).is_none_or(|owner| *owner == display)
        {
            remaps.push((before, space));
        }
        history.insert(display.to_string(), space);
    }
    remaps
}

/// The space a command acts on: the active display's, then the active space, then whatever space
/// the snapshot names, then whatever the one before named. A command has to land somewhere.
fn command_space(
    screens: &[ScreenInfo],
    active_display: Option<&str>,
    active_space: Option<SpaceId>,
    previous: &[ScreenInfo],
) -> Option<SpaceId> {
    active_display_space(screens, active_display, active_space)
        .or_else(|| previous.iter().find_map(|screen| screen.space))
}

pub(crate) fn active_display_space(
    screens: &[ScreenInfo],
    active_display: Option<&str>,
    active_space: Option<SpaceId>,
) -> Option<SpaceId> {
    active_display
        .and_then(|uuid| screens.iter().find(|screen| screen.display_uuid == uuid))
        .and_then(|screen| screen.space)
        .or_else(|| active_space.filter(|space| shown(screens, *space)))
        .or_else(|| screens.iter().find_map(|screen| screen.space))
}

/// The active space when a screen shows it, then whatever space the snapshot names, then whatever
/// the one before named.
fn menu_bar_space(
    screens: &[ScreenInfo],
    active_space: Option<SpaceId>,
    previous: &[ScreenInfo],
) -> Option<SpaceId> {
    active_space
        .filter(|space| shown(screens, *space))
        .or_else(|| screens.iter().find_map(|screen| screen.space))
        .or_else(|| previous.iter().find_map(|screen| screen.space))
}

fn shown(screens: &[ScreenInfo], space: SpaceId) -> bool {
    screens.iter().any(|screen| screen.space == Some(space))
}

/// The spaces each display owns, and whether they came from the screens. The window server's list,
/// or the screens' own spaces when it did not answer: an empty list would say a display owns no
/// spaces, which is never true of an attached display.
fn spaces_per_display(
    listed: HashMap<String, Vec<SpaceId>>,
    screens: &[ScreenInfo],
) -> (HashMap<String, Vec<SpaceId>>, bool) {
    if !listed.is_empty() {
        return (listed, false);
    }
    let mut from_screens: HashMap<String, Vec<SpaceId>> = HashMap::default();
    for screen in screens {
        if let Some(space) = screen.space {
            from_screens.entry(screen.display_uuid.clone()).or_default().push(space);
        }
    }
    (from_screens, true)
}

/// The spaces `screens` show, each once, in screen order.
pub fn active_spaces(screens: &[ScreenInfo]) -> Vec<SpaceId> {
    let mut seen = HashSet::default();
    screens
        .iter()
        .filter_map(|screen| screen.space)
        .filter(|space| seen.insert(*space))
        .collect()
}

/// An active space, with each window the window server lists on it and the space that window calls
/// home.
pub type Listing = (SpaceId, Vec<(WindowServerId, Option<SpaceId>)>);

/// Which active space each window is on, from each active space's `Listing`.
///
/// A space's own list is the authority; the home space only settles a window two lists name, and
/// when that read did not answer, the window keeps the active space it was on before. With
/// `keep_when_empty`, a listing of no windows at all keeps the windows `previous` had on the active
/// spaces: the first snapshot after wake or unlock can race the window server and list none.
pub fn visible_window_spaces(
    listed: &[Listing],
    previous: &HashMap<WindowServerId, SpaceId>,
    keep_when_empty: bool,
) -> HashMap<WindowServerId, SpaceId> {
    let active: HashSet<SpaceId> = listed.iter().map(|(space, _)| *space).collect();
    let on_active = |space: &SpaceId| active.contains(space);
    let mut visible: HashMap<WindowServerId, SpaceId> = HashMap::default();
    for (space, windows) in listed {
        for &(wsid, home) in windows {
            let resolved = match visible.get(&wsid) {
                None => *space,
                Some(seen) if seen == space => continue,
                Some(&seen) => home
                    .filter(on_active)
                    .or_else(|| previous.get(&wsid).copied().filter(on_active))
                    .unwrap_or(seen),
            };
            visible.insert(wsid, resolved);
        }
    }
    if visible.is_empty() && keep_when_empty {
        return previous
            .iter()
            .filter(|(_, space)| on_active(space))
            .map(|(&w, &s)| (w, s))
            .collect();
    }
    visible
}

/// The windows a display reconfiguration moved: each window `current` has and `previous` did not
/// as appeared, each `previous` had and `current` does not as disappeared, and a window that moved
/// space as both.
pub fn window_delta(
    epoch: u64,
    flags: DisplayReconfigFlags,
    previous: &HashMap<WindowServerId, SpaceId>,
    current: &HashMap<WindowServerId, SpaceId>,
) -> TopologyWindowDelta {
    let mut appeared = Vec::new();
    let mut disappeared = Vec::new();
    for (&wsid, &space) in current {
        match previous.get(&wsid).copied() {
            None => appeared.push((wsid, space)),
            Some(before) if before != space => {
                disappeared.push((wsid, before));
                appeared.push((wsid, space));
            }
            Some(_) => {}
        }
    }
    for (&wsid, &space) in previous {
        if !current.contains_key(&wsid) {
            disappeared.push((wsid, space));
        }
    }
    TopologyWindowDelta {
        epoch,
        flags,
        appeared,
        disappeared,
    }
}

#[cfg(test)]
mod tests;
