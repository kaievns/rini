//! How a window's decorations are painted: the hairline's white line and outline, the shadow and
//! the border, each by whether the window holds focus; the focus ring, by whether the window wears
//! it and whether its display holds focus; who wears it, and who is drawn urgent.

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
    pub fn corner(&self, reported: impl FnOnce() -> Option<f64>) -> Option<f64> {
        self.radius.or_else(reported)
    }
}

/// Whether a window wears the ring, and in which paint: its active paint while its workspace is on
/// the display holding focus, as niri paints every ring on the focused monitor active, else its
/// inactive one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ring {
    #[default]
    None,
    Inactive,
    Active,
}

impl Ring {
    pub fn worn(self) -> bool { self != Ring::None }
}

/// What a window wears through a pass: its decoration, and where the pass ends the ring it wears
/// (`wears_ring`) and whether it holds focus (`holds_focus`), which paints its border.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Worn {
    pub decoration: Decoration,
    pub wears_ring: Ring,
    pub holds_focus: bool,
}

impl Worn {
    /// How a tile wearing this is painted, drawn `focused` or not: its white line and shadow as
    /// drawn, which turn with its picture, and its ring and border as where the pass ends, from the
    /// flight's show to its lift.
    pub fn style(&self, focused: bool) -> Style {
        Style {
            light_alpha: light_alpha(focused),
            shadow: shadow(focused),
            ..Style::of(self.holds_focus, self.wears_ring, &self.decoration)
        }
    }
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
    /// ring (`ring`) wears its decoration's, in the urgent paint while it is drawn urgent, else in
    /// the paint `ring` names; any other wears none. The border is worn by every window its
    /// decoration has one for, in the urgent paint, else the active one while the window is focused
    /// and the inactive one otherwise, as niri paints it.
    pub fn of(focused: bool, ring: Ring, decoration: &Decoration) -> Style {
        let urgent = decoration.urgent;
        let ring_paint = decoration
            .ring
            .filter(|_| ring.worn())
            .map(|band| band.paint(ring == Ring::Active, urgent));
        Style {
            light_alpha: light_alpha(focused),
            shadow: shadow(focused),
            ring: ring_paint,
            border: decoration.border.map(|band| band.paint(focused, urgent)),
        }
    }
}

fn light_alpha(focused: bool) -> f64 { if focused { 0.20 } else { 0.18 } }

fn shadow(focused: bool) -> ShadowStyle {
    if focused {
        FOCUSED_SHADOW
    } else {
        UNFOCUSED_SHADOW
    }
}

/// Who wears the ring and in which paint, as niri has it (`niri:src/layout/scrolling.rs:2992-2995`,
/// `niri:src/layout/monitor.rs:1095-1100`): each workspace's active window, on every display, and
/// nothing else, in the active paint on the display holding focus and the inactive one elsewhere.
/// `workspaces` names each workspace with its display and its active window, its tiled selection or
/// the floating window focused last; `focus` is the managed window focused, its display and its
/// workspace, which on that workspace is the active window, floating or not. With no managed window
/// focused, no window holds focus, and every ring is the inactive one.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Rings {
    holders: Vec<(WindowId, Ring)>,
    focus: Option<WindowId>,
}

impl Rings {
    pub fn of<D: PartialEq, K: PartialEq>(
        workspaces: &[(D, K, Option<WindowId>)],
        focus: Option<(D, K, WindowId)>,
    ) -> Rings {
        let lit = |display: &D| focus.as_ref().is_some_and(|(focused, _, _)| focused == display);
        let mut holders: Vec<(WindowId, Ring)> = workspaces
            .iter()
            .filter(|(display, workspace, _)| {
                focus
                    .as_ref()
                    .is_none_or(|(on, focused, _)| (on, focused) != (display, workspace))
            })
            .filter_map(|(display, _, active)| {
                Some((
                    active.as_ref().copied()?,
                    if lit(display) {
                        Ring::Active
                    } else {
                        Ring::Inactive
                    },
                ))
            })
            .collect();
        holders.extend(focus.as_ref().map(|(_, _, window)| (*window, Ring::Active)));
        Rings {
            holders,
            focus: focus.map(|(_, _, window)| window),
        }
    }

    /// The ring `window` wears.
    pub fn ring(&self, window: WindowId) -> Ring {
        self.holders
            .iter()
            .find(|(holder, _)| *holder == window)
            .map_or(Ring::None, |(_, ring)| *ring)
    }

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

    fn ringed() -> Decoration {
        Decoration {
            ring: Some(RING),
            ..Decoration::default()
        }
    }

    fn w(idx: u32) -> WindowId { WindowId::new(1, idx) }

    #[test]
    fn the_focused_windows_hairline_is_the_brighter() {
        assert_eq!(
            Style::of(true, Ring::None, &Decoration::default()).light_alpha,
            0.20
        );
        assert_eq!(
            Style::of(false, Ring::None, &Decoration::default()).light_alpha,
            0.18
        );
    }

    #[test]
    fn the_ring_is_worn_by_each_workspaces_active_window_on_every_display() {
        let rings = Rings::of(
            &[
                ("a", "left", Some(w(1))),
                ("b", "right", Some(w(2))),
                ("b", "empty", None),
            ],
            Some(("a", "left", w(1))),
        );
        assert!(rings.ring(w(1)).worn() && rings.ring(w(2)).worn());
        assert_eq!(
            rings.ring(w(3)),
            Ring::None,
            "a window no workspace has active wears none"
        );
    }

    #[test]
    fn only_the_display_holding_focus_wears_the_active_colour() {
        let rings = Rings::of(
            &[("a", "left", Some(w(1))), ("b", "right", Some(w(2)))],
            Some(("a", "left", w(1))),
        );
        let paint = |window| Style::of(rings.focused(window), rings.ring(window), &ringed()).ring;
        assert_eq!(paint(w(1)), Some(RING.active));
        assert_eq!(paint(w(2)), Some(RING.inactive));
    }

    /// niri paints every workspace's active window on the focused monitor active, so the workspace a
    /// switch leaves keeps its window's ring active as it slides away.
    #[test]
    fn every_ring_on_the_display_holding_focus_wears_the_active_colour() {
        let workspaces = [
            ("a", 1, Some(w(1))),
            ("a", 2, Some(w(2))),
            ("b", 3, Some(w(3))),
        ];
        let rings = Rings::of(&workspaces, Some(("a", 2, w(2))));
        assert_eq!([rings.ring(w(1)), rings.ring(w(2)), rings.ring(w(3))], [
            Ring::Active,
            Ring::Active,
            Ring::Inactive
        ]);
        assert!(!rings.focused(w(1)), "active, not focused");
        let style = Style::of(rings.focused(w(1)), rings.ring(w(1)), &Decoration {
            border: Some(BORDER),
            ..ringed()
        });
        assert_eq!(
            (style.ring, style.border),
            (Some(RING.active), Some(BORDER.inactive)),
            "the border is the window's own focus"
        );
    }

    #[test]
    fn a_focused_floating_window_wears_the_ring_instead_of_the_tiled_selection() {
        let rings = Rings::of(&[("a", "left", Some(w(1)))], Some(("a", "left", w(5))));
        assert_eq!(rings.ring(w(5)), Ring::Active);
        assert_eq!(
            rings.ring(w(1)),
            Ring::None,
            "the selection behind it wears none"
        );
    }

    #[test]
    fn with_no_managed_window_focused_nothing_is_active() {
        let rings =
            Rings::of::<_, _>(&[("a", "left", Some(w(1))), ("b", "right", Some(w(2)))], None);
        for window in [w(1), w(2)] {
            assert!(
                rings.ring(window) == Ring::Inactive && !rings.focused(window),
                "each workspace's active window still"
            );
            let style = Style::of(rings.focused(window), rings.ring(window), &ringed());
            assert_eq!(style.ring, Some(RING.inactive), "and none of them focused");
        }
    }

    #[test]
    fn a_ringed_window_wears_the_paint_its_ring_names() {
        assert_eq!(
            Style::of(false, Ring::Inactive, &ringed()).ring,
            Some(RING.inactive)
        );
        assert_eq!(Style::of(true, Ring::Active, &ringed()).ring, Some(RING.active));
        assert_eq!(
            Style::of(false, Ring::Active, &ringed()).ring,
            Some(RING.active),
            "on the display holding focus"
        );
        assert_eq!(
            Style::of(true, Ring::None, &ringed()).ring,
            None,
            "focused but not its workspace's active window"
        );
        assert_eq!(
            Style::of(true, Ring::Active, &Decoration::default()).ring,
            None,
            "rings off"
        );
    }

    #[test]
    fn the_border_wears_the_active_paint_only_while_focused() {
        let bordered = Decoration {
            border: Some(BORDER),
            ..ringed()
        };
        for ring in [Ring::Active, Ring::Inactive, Ring::None] {
            assert_eq!(
                Style::of(true, ring, &bordered).border,
                Some(BORDER.active),
                "{ring:?}"
            );
            assert_eq!(
                Style::of(false, ring, &bordered).border,
                Some(BORDER.inactive),
                "{ring:?}"
            );
        }
        assert_eq!(
            Style::of(true, Ring::Active, &ringed()).border,
            None,
            "borders off"
        );
    }

    /// Focus moves from window 1 to window 2: from the show, each tile's ring and border are what
    /// its window wears where the flight ends, drawn focused or not, and only its white line and
    /// shadow are what it is drawn. On one workspace the ring leaves window 1 and comes to window 2;
    /// to another workspace on the display, each is its workspace's active window and wears the
    /// active ring; across displays, window 1 keeps the inactive one.
    #[test]
    fn a_tile_wears_its_bands_as_where_the_flight_ends_whatever_its_picture_shows() {
        let bordered = Decoration {
            border: Some(BORDER),
            ..ringed()
        };
        let worn = |wears_ring, holds_focus| Worn {
            decoration: bordered,
            wears_ring,
            holds_focus,
        };
        let bands = |style: Style| (style.ring, style.border);
        for focused in [true, false] {
            assert_eq!(
                bands(worn(Ring::None, false).style(focused)),
                (None, Some(BORDER.inactive)),
                "losing, one workspace"
            );
            assert_eq!(
                bands(worn(Ring::Active, false).style(focused)),
                (Some(RING.active), Some(BORDER.inactive)),
                "losing, a switch"
            );
            assert_eq!(
                bands(worn(Ring::Inactive, false).style(focused)),
                (Some(RING.inactive), Some(BORDER.inactive)),
                "losing to another display"
            );
            assert_eq!(
                bands(worn(Ring::Active, true).style(focused)),
                (Some(RING.active), Some(BORDER.active)),
                "gaining"
            );
        }
        let losing = worn(Ring::None, false).style(true);
        assert_eq!(
            (losing.light_alpha, losing.shadow),
            (0.20, FOCUSED_SHADOW),
            "drawn focused until its turn"
        );
        let gaining = worn(Ring::Active, true).style(false);
        assert_eq!(
            (gaining.light_alpha, gaining.shadow),
            (0.18, UNFOCUSED_SHADOW),
            "drawn unfocused until its turn"
        );
        assert_eq!(
            Worn::default().style(true),
            Style::of(true, Ring::None, &Decoration::default()),
            "bands off"
        );
    }

    #[test]
    fn an_app_wanting_attention_draws_each_unfocused_window_urgent() {
        let (editor, other) = (|idx| WindowId::new(7, idx), |idx| WindowId::new(9, idx));
        let attention = Attention::of([7]);
        let rings = Rings::of(
            &[
                ("a", "left", Some(editor(1))),
                ("b", "right", Some(other(1))),
            ],
            Some(("b", "right", other(1))),
        );
        for window in [editor(1), editor(2)] {
            assert!(
                rings.urgent(window, &attention),
                "{window:?}: ringed or not, every window of the app"
            );
        }
        assert!(!rings.urgent(other(2), &attention), "an app that does not ask");
        assert!(
            Rings::default().urgent(editor(1), &attention),
            "with nothing focused too"
        );
    }

    #[test]
    fn focusing_the_app_clears_its_urgency() {
        let attention = Attention::of([7]);
        let rings = Rings::of(
            &[("a", "left", Some(WindowId::new(7, 1)))],
            Some(("a", "left", WindowId::new(7, 1))),
        );
        assert!(
            !rings.urgent(WindowId::new(7, 1), &attention),
            "the window holding focus"
        );
        assert!(
            !rings.urgent(WindowId::new(7, 2), &attention),
            "and every other window of its app"
        );
    }

    #[test]
    fn urgent_beats_active_beats_inactive() {
        let urgent = Decoration {
            border: Some(BORDER),
            urgent: true,
            ..ringed()
        };
        for (focused, ring) in [
            (true, Ring::Active),
            (false, Ring::Inactive),
            (false, Ring::Active),
        ] {
            let style = Style::of(focused, ring, &urgent);
            assert_eq!(
                (style.ring, style.border),
                (Some(RING.urgent), Some(BORDER.urgent)),
                "focused {focused}, {ring:?}"
            );
        }
        assert_eq!(
            Style::of(false, Ring::None, &urgent).ring,
            None,
            "urgent wears the ring only where it wears it"
        );
        assert_eq!(RING.paint(true, false), RING.active);
        assert_eq!(RING.paint(false, false), RING.inactive);
    }

    #[test]
    fn a_rule_radius_goes_ahead_of_the_reported_one() {
        let ruled = Decoration {
            radius: Some(12.0),
            ..Decoration::default()
        };
        assert_eq!(
            ruled.corner(|| panic!("no read while a rule gives the radius")),
            Some(12.0)
        );
        assert_eq!(Decoration::default().corner(|| Some(26.0)), Some(26.0));
        assert_eq!(
            Decoration::default().corner(|| None),
            None,
            "the fallback is shape's"
        );
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
        const { assert!(UNFOCUSED_SHADOW.offset_y > 0.0) };
        const { assert!(FOCUSED_SHADOW.offset_y > 0.0) };
    }

    #[test]
    fn focus_deepens_the_shadow_in_every_dimension() {
        const { assert!(FOCUSED_SHADOW.opacity > UNFOCUSED_SHADOW.opacity) };
        const { assert!(FOCUSED_SHADOW.radius > UNFOCUSED_SHADOW.radius) };
        const { assert!(FOCUSED_SHADOW.offset_y > UNFOCUSED_SHADOW.offset_y) };
    }

    #[test]
    fn focus_picks_the_deep_style() {
        assert_eq!(
            Style::of(true, Ring::None, &Decoration::default()).shadow,
            FOCUSED_SHADOW
        );
        assert_eq!(
            Style::of(false, Ring::None, &Decoration::default()).shadow,
            UNFOCUSED_SHADOW
        );
    }
}
