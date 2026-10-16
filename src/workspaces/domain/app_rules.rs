use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{SpaceId, WindowId};

use crate::windows::domain::rules::{AppRuleSize, FloatingPosition, RelativeTo};
use crate::workspaces::VirtualWorkspaceId;

/// Complete result of applying a managed app rule to workspace policy.
///
/// The layout engine consumes this as one unit so rule effects do not get
/// independently re-derived at each integration point.
#[derive(Debug, Clone, Copy)]
pub struct AppRuleEffects {
    pub workspace_id: VirtualWorkspaceId,
    pub floating: RuleFloating,
    pub position: Option<FloatingPosition>,
    pub size: Option<AppRuleSize>,
    pub focus: bool,
}

/// What the rules say about floating a window, given whether the last rule to decide it floated it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleFloating {
    /// A rule floats it.
    Float,
    /// No rule floats it, and the last one floated it: it goes back to the strip.
    Tile,
    /// No rule floats it, nor did the last one: it stays as it is.
    Keep,
}

impl RuleFloating {
    /// No rule floating the window, after a last rule that `floated` it or not.
    pub fn unfloated(floated: bool) -> Self { if floated { Self::Tile } else { Self::Keep } }

    /// Floating as the window already is, `floating` or not, with no rule having floated it before.
    pub fn as_is(floating: bool) -> Self { if floating { Self::Float } else { Self::Keep } }

    /// Whether a rule floats the window, which is what the next evaluation reads as the last rule.
    pub fn floats(self) -> bool { self == Self::Float }

    /// Whether the window floats, given whether it `was_floating`.
    pub fn should_float(self, was_floating: bool) -> bool {
        match self {
            Self::Float => true,
            Self::Tile => false,
            Self::Keep => was_floating,
        }
    }
}

impl AppRuleEffects {
    pub fn should_float(self, was_floating: bool) -> bool {
        self.floating.should_float(was_floating)
    }

    pub fn floating_placement(self, window: WindowId, space: SpaceId) -> Option<AppRulePlacement> {
        (self.floating.floats() && (self.position.is_some() || self.size.is_some())).then_some(
            AppRulePlacement {
                window,
                space,
                position: self.position,
                size: self.size,
            },
        )
    }

    pub fn tiled_resize(
        self,
        window: WindowId,
        space: SpaceId,
        was_floating: bool,
    ) -> Option<AppRuleResize> {
        (!self.should_float(was_floating)).then_some(AppRuleResize {
            window,
            space,
            workspace_id: self.workspace_id,
            size: self.size?,
        })
    }
}

/// Workspace-policy result for one evaluated window.
#[derive(Debug, Clone, Copy)]
pub enum AppRuleResult {
    Managed(AppRuleEffects),
    Unmanaged,
}

/// Follow-up integration work produced while applying a batch of app rules.
///
/// Like the reactor's `EventOutcome`, this is returned to the owning layer and
/// consumed explicitly rather than stored as transient engine state.
#[derive(Debug, Default)]
pub struct AppRuleOutcome {
    placements: Vec<AppRulePlacement>,
    resizes: Vec<AppRuleResize>,
    workspace_focus: Option<AppRuleWorkspaceFocus>,
}

impl AppRuleOutcome {
    pub fn push_placement(&mut self, placement: AppRulePlacement) {
        self.placements.push(placement);
    }

    pub fn push_resize(&mut self, resize: AppRuleResize) { self.resizes.push(resize); }

    pub fn has_resizes(&self) -> bool { !self.resizes.is_empty() }

    pub fn set_workspace_focus(&mut self, focus: AppRuleWorkspaceFocus) {
        self.workspace_focus = Some(focus);
    }

    pub fn into_parts(
        self,
    ) -> (
        Vec<AppRulePlacement>,
        Vec<AppRuleResize>,
        Option<AppRuleWorkspaceFocus>,
    ) {
        (self.placements, self.resizes, self.workspace_focus)
    }
}

/// One-shot frame request derived from a floating app rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppRulePlacement {
    pub window: WindowId,
    pub space: SpaceId,
    pub position: Option<FloatingPosition>,
    pub size: Option<AppRuleSize>,
}

impl AppRulePlacement {
    pub fn resolve_frame(self, current: CGRect, screen: CGRect) -> CGRect {
        let mut frame = current;
        if let Some(size) = self.size {
            if let Some(width) = size.w {
                frame.size.width = width;
            }
            if let Some(height) = size.h {
                frame.size.height = height;
            }
        }
        if let Some(position) = self.position {
            frame.origin = floating_origin(position, frame.size.width, frame.size.height, screen);
        }
        frame
    }
}

/// niri's `default-floating-position`: logical pixels from the corner `relative_to` names, each
/// coordinate counted away from that corner, or from the middle of a side when only a side is named.
fn floating_origin(position: FloatingPosition, width: f64, height: f64, screen: CGRect) -> CGPoint {
    use RelativeTo::*;
    let (left, top) = (screen.origin.x, screen.origin.y);
    let right = screen.origin.x + screen.size.width - width;
    let bottom = screen.origin.y + screen.size.height - height;
    let middle_x = screen.origin.x + (screen.size.width - width) / 2.0;
    let middle_y = screen.origin.y + (screen.size.height - height) / 2.0;
    let (x, y) = (position.x, position.y);
    let (ox, oy) = match position.relative_to {
        TopLeft => (left + x, top + y),
        TopRight => (right - x, top + y),
        BottomLeft => (left + x, bottom - y),
        BottomRight => (right - x, bottom - y),
        Top => (middle_x + x, top + y),
        Bottom => (middle_x + x, bottom - y),
        Left => (left + x, middle_y + y),
        Right => (right - x, middle_y + y),
    };
    CGPoint::new(ox, oy)
}

/// One-time tiled resize applied after the window has entered its layout tree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppRuleResize {
    pub window: WindowId,
    pub space: SpaceId,
    pub workspace_id: VirtualWorkspaceId,
    pub size: AppRuleSize,
}

impl AppRuleResize {
    /// `current` with whichever dimensions the rule names replaced.
    pub fn resized_frame(&self, current: CGRect) -> CGRect {
        let mut frame = current;
        if let Some(width) = self.size.w {
            frame.size.width = width;
        }
        if let Some(height) = self.size.h {
            frame.size.height = height;
        }
        frame
    }
}

/// What the layout knew about a window before its app rules were re-evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeforeRules {
    pub assigned: bool,
    pub floating: bool,
    pub ignored: bool,
}

/// What the layout has to do once a window's rules have been re-evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterRules {
    /// Its place in the layout changed (newly assigned, float flipped, or ignored no more).
    RefreshLayout,
    /// The rules now leave it unmanaged while the layout still holds it.
    RemoveFromLayout,
    Settled,
}

impl AfterRules {
    /// The follow-up for a rule result. `in_layout_now` is whether the layout still holds the
    /// window after the evaluation; it only matters for an unmanaged verdict.
    pub fn for_result(
        before: BeforeRules,
        result: Result<&AppRuleResult, ()>,
        in_layout_now: bool,
    ) -> Self {
        match result {
            Ok(AppRuleResult::Managed(effects)) => {
                let floating = effects.should_float(before.floating);
                if !before.assigned || before.floating != floating || before.ignored {
                    Self::RefreshLayout
                } else {
                    Self::Settled
                }
            }
            Ok(AppRuleResult::Unmanaged) => {
                if in_layout_now {
                    Self::RemoveFromLayout
                } else {
                    Self::Settled
                }
            }
            Err(()) => {
                if !before.assigned || before.ignored {
                    Self::RefreshLayout
                } else {
                    Self::Settled
                }
            }
        }
    }
}

/// Reactor-owned part of a focus rule: switching workspaces requires saving
/// the currently visible floating frames before the engine activates the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppRuleWorkspaceFocus {
    pub window: WindowId,
    pub space: SpaceId,
    pub workspace_index: usize,
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn effects(floating: RuleFloating) -> AppRuleResult {
        AppRuleResult::Managed(AppRuleEffects {
            workspace_id: VirtualWorkspaceId::default(),
            floating,
            position: None,
            size: None,
            focus: false,
        })
    }

    /// Each verdict against each state the window was in, and the verdict each rule and last rule
    /// gives.
    #[test]
    fn a_rule_floats_a_window_tiles_one_the_last_rule_floated_and_otherwise_keeps_it() {
        use RuleFloating::{Float, Keep, Tile};
        let floats = [
            (Float, false, true),
            (Float, true, true),
            (Tile, false, false),
            (Tile, true, false),
            (Keep, false, false),
            (Keep, true, true),
        ];
        for (verdict, was_floating, floating) in floats {
            assert_eq!(
                verdict.should_float(was_floating),
                floating,
                "{verdict:?} {was_floating}"
            );
        }
        assert_eq!(RuleFloating::unfloated(true), Tile);
        assert_eq!(RuleFloating::unfloated(false), Keep);
        assert_eq!(RuleFloating::as_is(true), Float);
        assert_eq!(RuleFloating::as_is(false), Keep);
        assert_eq!([Float, Tile, Keep].map(RuleFloating::floats), [
            true, false, false
        ]);
    }

    fn placed(relative_to: RelativeTo) -> CGPoint {
        let placement = AppRulePlacement {
            window: WindowId::new(1, 1),
            space: SpaceId::new(1),
            position: Some(FloatingPosition { x: 40.0, y: 30.0, relative_to }),
            size: Some(AppRuleSize { w: Some(200.0), h: Some(100.0) }),
        };
        let screen = CGRect::new(CGPoint::new(0.0, 32.0), CGSize::new(1000.0, 600.0));
        let current = CGRect::new(CGPoint::new(5.0, 5.0), CGSize::new(10.0, 10.0));
        placement.resolve_frame(current, screen).origin
    }

    /// Each coordinate counts away from the corner named, as niri's do: `bottom-left` puts `y` above
    /// the bottom edge.
    #[test]
    fn a_floating_position_is_pixels_from_the_corner_it_names() {
        assert_eq!(placed(RelativeTo::TopLeft), CGPoint::new(40.0, 62.0));
        assert_eq!(placed(RelativeTo::TopRight), CGPoint::new(760.0, 62.0));
        assert_eq!(placed(RelativeTo::BottomLeft), CGPoint::new(40.0, 502.0));
        assert_eq!(placed(RelativeTo::BottomRight), CGPoint::new(760.0, 502.0));
    }

    #[test]
    fn a_side_alone_centres_the_window_along_it() {
        assert_eq!(placed(RelativeTo::Top), CGPoint::new(440.0, 62.0));
        assert_eq!(placed(RelativeTo::Bottom), CGPoint::new(440.0, 502.0));
        assert_eq!(placed(RelativeTo::Left), CGPoint::new(40.0, 312.0));
        assert_eq!(placed(RelativeTo::Right), CGPoint::new(760.0, 312.0));
    }

    #[test]
    fn a_resize_replaces_only_the_dimensions_the_rule_names() {
        let resize = AppRuleResize {
            window: WindowId::new(1, 1),
            space: SpaceId::new(1),
            workspace_id: VirtualWorkspaceId::default(),
            size: AppRuleSize { w: Some(500.0), h: None },
        };
        let current = CGRect::new(CGPoint::new(10.0, 20.0), CGSize::new(300.0, 400.0));
        let frame = resize.resized_frame(current);
        assert_eq!(frame.origin, current.origin);
        assert_eq!(frame.size, CGSize::new(500.0, 400.0));
    }

    #[test]
    fn a_managed_verdict_refreshes_when_the_windows_place_changes() {
        let settled = BeforeRules {
            assigned: true,
            floating: false,
            ignored: false,
        };
        assert_eq!(
            AfterRules::for_result(settled, Ok(&effects(RuleFloating::Tile)), true),
            AfterRules::Settled
        );
        assert_eq!(
            AfterRules::for_result(settled, Ok(&effects(RuleFloating::Float)), true),
            AfterRules::RefreshLayout
        );
        let unassigned = BeforeRules { assigned: false, ..settled };
        assert_eq!(
            AfterRules::for_result(unassigned, Ok(&effects(RuleFloating::Tile)), true),
            AfterRules::RefreshLayout
        );
        let ignored = BeforeRules { ignored: true, ..settled };
        assert_eq!(
            AfterRules::for_result(ignored, Ok(&effects(RuleFloating::Tile)), true),
            AfterRules::RefreshLayout
        );
    }

    #[test]
    fn a_rule_that_says_nothing_about_floating_keeps_the_windows_current_state() {
        let floating = BeforeRules {
            assigned: true,
            floating: true,
            ignored: false,
        };
        assert_eq!(
            AfterRules::for_result(floating, Ok(&effects(RuleFloating::Keep)), true),
            AfterRules::Settled
        );
        assert_eq!(
            AfterRules::for_result(floating, Ok(&effects(RuleFloating::Tile)), true),
            AfterRules::RefreshLayout
        );
    }

    #[test]
    fn an_unmanaged_verdict_removes_only_what_the_layout_still_holds() {
        let before = BeforeRules {
            assigned: true,
            floating: false,
            ignored: false,
        };
        assert_eq!(
            AfterRules::for_result(before, Ok(&AppRuleResult::Unmanaged), true),
            AfterRules::RemoveFromLayout
        );
        assert_eq!(
            AfterRules::for_result(before, Ok(&AppRuleResult::Unmanaged), false),
            AfterRules::Settled
        );
    }

    #[test]
    fn a_failed_evaluation_refreshes_a_window_the_layout_did_not_have() {
        let held = BeforeRules {
            assigned: true,
            floating: false,
            ignored: false,
        };
        assert_eq!(AfterRules::for_result(held, Err(()), true), AfterRules::Settled);
        assert_eq!(
            AfterRules::for_result(BeforeRules { assigned: false, ..held }, Err(()), true),
            AfterRules::RefreshLayout
        );
        assert_eq!(
            AfterRules::for_result(BeforeRules { ignored: true, ..held }, Err(()), true),
            AfterRules::RefreshLayout
        );
    }
}
