//! How a window's decorations are painted: the hairline's white line and outline, the shadow and
//! the border, each by whether the window holds focus; the focus ring, by whether the window wears
//! it; who wears it, and who is drawn urgent.

use rini_core::ids::WindowId;
use serde::Serialize;

use crate::decorations::domain::paint::Paint;
use crate::windows::domain::attention::Attention;

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

/// A band rini draws outside a window, as niri's focus ring and border are: its width in points, and
/// its paint, a colour or a gradient, on the window holding focus, on a window drawn urgent and on
/// the others that wear it.
#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
pub struct Band {
    pub width: f64,
    pub active: Paint,
    pub inactive: Paint,
    pub urgent: Paint,
}

impl Band {
    /// Its paint on a window drawn urgent, else holding focus, else neither, as niri picks it
    /// (`niri:src/layout/focus_ring.rs:73-98`).
    pub fn paint(&self, focused: bool, urgent: bool) -> Paint {
        if urgent {
            self.urgent
        } else if focused {
            self.active
        } else {
            self.inactive
        }
    }
}

/// What a window wears that rini draws rather than macOS: its focus ring and its border, each while
/// it is on for the window, the corner a window rule gives the two, and whether it is drawn urgent.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Decoration {
    pub ring: Option<Band>,
    pub border: Option<Band>,
    /// `geometry-corner-radius`, in points.
    pub radius: Option<f64>,
    /// Its application asks for attention and is not the one holding focus (`Rings::urgent`).
    pub urgent: bool,
}

impl Decoration {
    /// The radius its ring and border round to: its rule's, else `reported`, the one the window
    /// server reports, read only when no rule gives one. What macOS draws at rest, the hairline and
    /// the shadow, keeps the window server's on a tile too.
    pub fn corner(&self, reported: impl FnOnce() -> Option<f64>) -> Option<f64> { self.radius.or_else(reported) }
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
/// and the border's paint, for each it wears.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Style {
    pub light_alpha: f64,
    pub shadow: ShadowStyle,
    pub ring: Option<Paint>,
    pub border: Option<Paint>,
}

impl Style {
    /// A focused window's: the brighter white line and the deeper shadow. A window that wears the
    /// ring (`ringed`) wears its decoration's, in the urgent paint while it is drawn urgent, the
    /// active one while it is focused and the inactive one otherwise; any other wears none. The
    /// border is worn by every window its decoration has one for, painted the same way, as niri
    /// paints it.
    pub fn of(focused: bool, ringed: bool, decoration: &Decoration) -> Style {
        let paint = |band: Band| band.paint(focused, decoration.urgent);
        let ring = decoration.ring.filter(|_| ringed).map(paint);
        let (light_alpha, shadow) =
            if focused { (0.20, FOCUSED_SHADOW) } else { (0.18, UNFOCUSED_SHADOW) };
        Style { light_alpha, shadow, ring, border: decoration.border.map(paint) }
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
    focus: Option<WindowId>,
}

impl Rings {
    pub fn of<K: PartialEq>(workspaces: &[(K, Option<WindowId>)], focus: Option<(K, WindowId)>) -> Rings {
        let mut holders: Vec<WindowId> = workspaces
            .iter()
            .filter(|(workspace, _)| focus.as_ref().is_none_or(|(focused, _)| focused != workspace))
            .filter_map(|(_, active)| *active)
            .collect();
        holders.extend(focus.as_ref().map(|(_, window)| *window));
        Rings { holders, focus: focus.map(|(_, window)| window) }
    }

    /// Whether `window` wears the ring.
    pub fn ringed(&self, window: WindowId) -> bool { self.holders.contains(&window) }

    /// Whether `window` holds focus, so wears the active paint.
    pub fn focused(&self, window: WindowId) -> bool { self.focus == Some(window) }

    /// Whether `window` is drawn urgent: its application asks for attention, and every window of
    /// it is, but while one of its windows holds focus. macOS asks per application, and activating
    /// the application ends the ask, as focusing a window ends niri's.
    pub fn urgent(&self, window: WindowId, attention: &Attention) -> bool {
        attention.asks(window.pid) && self.focus.is_none_or(|focus| focus.pid != window.pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decorations::domain::paint::Colour;
    use crate::decorations::domain::shape::SHADOW_REACH;

    const RING: Band = Band {
        width: 4.0,
        active: Paint::Solid(Colour::rgb(0x7fc8ff)),
        inactive: Paint::Solid(Colour::rgb(0x505050)),
        urgent: Paint::Solid(Colour::rgb(0x9b0000)),
    };

    const BORDER: Band = Band {
        width: 4.0,
        active: Paint::Solid(Colour::rgb(0xffc87f)),
        inactive: Paint::Solid(Colour::rgb(0x505050)),
        urgent: Paint::Solid(Colour::rgb(0x9b0000)),
    };

    fn ringed() -> Decoration { Decoration { ring: Some(RING), ..Decoration::default() } }

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
    fn only_the_display_holding_focus_wears_the_active_colour() {
        let rings = Rings::of(&[("left", Some(w(1))), ("right", Some(w(2)))], Some(("left", w(1))));
        let paint = |window| Style::of(rings.focused(window), rings.ringed(window), &ringed()).ring;
        assert_eq!(paint(w(1)), Some(RING.active));
        assert_eq!(paint(w(2)), Some(RING.inactive));
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
        for window in [w(1), w(2)] {
            assert!(rings.ringed(window) && !rings.focused(window), "each workspace's active window still");
            let style = Style::of(rings.focused(window), rings.ringed(window), &ringed());
            assert_eq!(style.ring, Some(RING.inactive), "and none of them focused");
        }
    }

    #[test]
    fn a_ringed_window_not_focused_wears_the_inactive_paint() {
        assert_eq!(Style::of(false, true, &ringed()).ring, Some(RING.inactive));
        assert_eq!(Style::of(true, true, &ringed()).ring, Some(RING.active));
        assert_eq!(Style::of(true, false, &ringed()).ring, None, "focused but not its workspace's active window");
        assert_eq!(Style::of(true, true, &Decoration::default()).ring, None, "rings off");
    }

    #[test]
    fn the_border_wears_the_active_paint_only_while_focused() {
        let bordered = Decoration { border: Some(BORDER), ..ringed() };
        for ringed in [true, false] {
            assert_eq!(Style::of(true, ringed, &bordered).border, Some(BORDER.active), "ringed {ringed}");
            assert_eq!(Style::of(false, ringed, &bordered).border, Some(BORDER.inactive), "ringed {ringed}");
        }
        assert_eq!(Style::of(true, true, &ringed()).border, None, "borders off");
    }

    #[test]
    fn an_app_wanting_attention_draws_each_unfocused_window_urgent() {
        let (editor, other) = (|idx| WindowId::new(7, idx), |idx| WindowId::new(9, idx));
        let attention = Attention::of([7]);
        let rings = Rings::of(&[("left", Some(editor(1))), ("right", Some(other(1)))], Some(("right", other(1))));
        for window in [editor(1), editor(2)] {
            assert!(rings.urgent(window, &attention), "{window:?}: ringed or not, every window of the app");
        }
        assert!(!rings.urgent(other(2), &attention), "an app that does not ask");
        assert!(Rings::default().urgent(editor(1), &attention), "with nothing focused too");
    }

    #[test]
    fn focusing_the_app_clears_its_urgency() {
        let attention = Attention::of([7]);
        let rings = Rings::of(&[("left", Some(WindowId::new(7, 1)))], Some(("left", WindowId::new(7, 1))));
        assert!(!rings.urgent(WindowId::new(7, 1), &attention), "the window holding focus");
        assert!(!rings.urgent(WindowId::new(7, 2), &attention), "and every other window of its app");
    }

    #[test]
    fn urgent_beats_active_beats_inactive() {
        let urgent = Decoration { border: Some(BORDER), urgent: true, ..ringed() };
        for focused in [true, false] {
            let style = Style::of(focused, true, &urgent);
            assert_eq!((style.ring, style.border), (Some(RING.urgent), Some(BORDER.urgent)), "focused {focused}");
        }
        assert_eq!(Style::of(false, false, &urgent).ring, None, "urgent wears the ring only where it wears it");
        assert_eq!(RING.paint(true, false), RING.active);
        assert_eq!(RING.paint(false, false), RING.inactive);
    }

    #[test]
    fn a_rule_radius_goes_ahead_of_the_reported_one() {
        let ruled = Decoration { radius: Some(12.0), ..Decoration::default() };
        assert_eq!(ruled.corner(|| panic!("no read while a rule gives the radius")), Some(12.0));
        assert_eq!(Decoration::default().corner(|| Some(26.0)), Some(26.0));
        assert_eq!(Decoration::default().corner(|| None), None, "the fallback is shape's");
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
