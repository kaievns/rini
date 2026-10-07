//! How a window's decorations are painted: the hairline's white line and outline, and the shadow,
//! each by whether the window holds focus; the focus ring, by whether the window wears it; and who
//! wears it.

use rini_core::ids::WindowId;
use serde::Serialize;

use crate::decorations::domain::paint::Colour;

/// The white line's width in points: two device pixels at 2x.
pub const LIGHT_PT: f64 = 1.0;

/// The outline's stroke width in points, and its black's alpha.
pub const OUTLINE_PT: f64 = 0.5;
pub const OUTLINE_ALPHA: f64 = 0.9;

/// The shadow a real window casts, which no capture API includes. Fitted to measured falloffs;
/// see "Shadows are never in the surface" in `src/animation/docs/capture-overlay-research.md`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ShadowStyle {
    pub opacity: f32,
    pub radius: f64,
    /// Positive is downward: the tiles hang off a flipped view.
    pub offset_y: f64,
}

const UNFOCUSED_SHADOW: ShadowStyle = ShadowStyle {
    opacity: 0.4,
    radius: 9.0,
    offset_y: 5.0,
};
const FOCUSED_SHADOW: ShadowStyle = ShadowStyle {
    opacity: 0.65,
    radius: 14.0,
    offset_y: 12.0,
};

/// A band rini draws outside a window, as niri's focus ring is: its width in points, and its paint
/// on the window holding focus and on the others that wear it.
#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
pub struct Band {
    pub width: f64,
    pub active: Colour,
    pub inactive: Colour,
}

/// What a window wears that rini draws rather than macOS: its focus ring, while rings are on.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Decoration {
    pub ring: Option<Band>,
}

/// What a window wears through a pass: its decoration, and whether it wears the ring before the pass
/// (`wore_ring`) and where the pass ends (`wears_ring`).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Worn {
    pub decoration: Decoration,
    pub wore_ring: bool,
    pub wears_ring: bool,
}

/// How a window's decorations are painted: the alpha of the white line, the shadow, and the ring's
/// colour, if it wears one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Style {
    pub light_alpha: f64,
    pub shadow: ShadowStyle,
    pub ring: Option<Colour>,
}

impl Style {
    /// A focused window's: the brighter white line and the deeper shadow. A window that wears the
    /// ring (`ringed`) wears its decoration's, in the active paint while it is focused and the
    /// inactive one otherwise; any other wears none.
    pub fn of(focused: bool, ringed: bool, decoration: &Decoration) -> Style {
        let ring = decoration
            .ring
            .filter(|_| ringed)
            .map(|band| if focused { band.active } else { band.inactive });
        let (light_alpha, shadow) =
            if focused { (0.20, FOCUSED_SHADOW) } else { (0.18, UNFOCUSED_SHADOW) };
        Style { light_alpha, shadow, ring }
    }
}

/// Who wears the ring, as niri has it (`niri:src/layout/scrolling.rs:2992-2995`): each workspace's
/// active window, on every display, and nothing else. `workspaces` names each workspace with its
/// active window, its tiled selection or the floating window focused last; `focus` is the managed
/// window focused and its workspace, which on that workspace is the active window, floating or not.
/// With no managed window focused, no window holds focus, and every ring is the inactive one.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Rings {
    holders: Vec<WindowId>,
}

impl Rings {
    pub fn of<K: PartialEq>(workspaces: &[(K, Option<WindowId>)], focus: Option<(K, WindowId)>) -> Rings {
        let mut holders: Vec<WindowId> = workspaces
            .iter()
            .filter(|(workspace, _)| focus.as_ref().is_none_or(|(focused, _)| focused != workspace))
            .filter_map(|(_, active)| *active)
            .collect();
        holders.extend(focus.as_ref().map(|(_, window)| *window));
        Rings { holders }
    }

    /// Whether `window` wears the ring.
    pub fn ringed(&self, window: WindowId) -> bool { self.holders.contains(&window) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decorations::domain::shape::SHADOW_REACH;

    const RING: Band = Band {
        width: 4.0,
        active: Colour::rgb(0x7fc8ff),
        inactive: Colour::rgb(0x505050),
    };

    fn ringed() -> Decoration { Decoration { ring: Some(RING) } }

    fn w(idx: u32) -> WindowId { WindowId::new(1, idx) }

    #[test]
    fn the_focused_windows_hairline_is_the_brighter() {
        assert_eq!(Style::of(true, false, &Decoration::default()).light_alpha, 0.20);
        assert_eq!(Style::of(false, false, &Decoration::default()).light_alpha, 0.18);
    }

    #[test]
    fn the_ring_is_worn_by_each_workspaces_active_window_on_every_display() {
        let rings = Rings::of(&[("left", Some(w(1))), ("right", Some(w(2))), ("empty", None)], Some(("left", w(1))));
        assert!(rings.ringed(w(1)) && rings.ringed(w(2)));
        assert!(!rings.ringed(w(3)), "a window no workspace has active wears none");
    }

    #[test]
    fn a_focused_floating_window_wears_the_ring_instead_of_the_tiled_selection() {
        let rings = Rings::of(&[("left", Some(w(1)))], Some(("left", w(5))));
        assert!(rings.ringed(w(5)));
        assert!(!rings.ringed(w(1)), "the selection behind it wears none");
    }

    #[test]
    fn with_no_managed_window_focused_nothing_is_active() {
        let rings = Rings::of(&[("left", Some(w(1))), ("right", Some(w(2)))], None);
        assert!(rings.ringed(w(1)) && rings.ringed(w(2)), "each workspace's active window still");
        assert_eq!(Style::of(false, true, &ringed()).ring, Some(RING.inactive), "and none of them focused");
    }

    #[test]
    fn a_ringed_window_not_focused_wears_the_inactive_paint() {
        assert_eq!(Style::of(false, true, &ringed()).ring, Some(RING.inactive));
        assert_eq!(Style::of(true, true, &ringed()).ring, Some(RING.active));
        assert_eq!(Style::of(true, false, &ringed()).ring, None, "focused but not its workspace's active window");
        assert_eq!(Style::of(true, true, &Decoration::default()).ring, None, "rings off");
    }

    /// Three radii plus the offset is where a gaussian ramp is visually spent.
    #[test]
    fn the_mask_reaches_further_than_either_shadow_does() {
        for style in [UNFOCUSED_SHADOW, FOCUSED_SHADOW] {
            assert!(SHADOW_REACH > style.radius * 3.0 + style.offset_y);
        }
    }

    #[test]
    fn the_shadows_fall_downward() {
        // Flipped view: positive y is down the screen.
        assert!(UNFOCUSED_SHADOW.offset_y > 0.0);
        assert!(FOCUSED_SHADOW.offset_y > 0.0);
    }

    #[test]
    fn focus_deepens_the_shadow_in_every_dimension() {
        assert!(FOCUSED_SHADOW.opacity > UNFOCUSED_SHADOW.opacity);
        assert!(FOCUSED_SHADOW.radius > UNFOCUSED_SHADOW.radius);
        assert!(FOCUSED_SHADOW.offset_y > UNFOCUSED_SHADOW.offset_y);
    }

    #[test]
    fn focus_picks_the_deep_style() {
        assert_eq!(Style::of(true, false, &Decoration::default()).shadow, FOCUSED_SHADOW);
        assert_eq!(Style::of(false, false, &Decoration::default()).shadow, UNFOCUSED_SHADOW);
    }
}
