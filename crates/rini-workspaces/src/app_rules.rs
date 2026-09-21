use objc2_core_foundation::{CGPoint, CGRect};

use rini_core::ids::WindowId;
use rini_windows::rules::{AppRulePosition, AppRuleSize};
use crate::VirtualWorkspaceId;
use rini_core::ids::SpaceId;

/// Complete result of applying a managed app rule to workspace policy.
///
/// The layout engine consumes this as one unit so rule effects do not get
/// independently re-derived at each integration point.
#[derive(Debug, Clone, Copy)]
pub struct AppRuleEffects {
    pub workspace_id: VirtualWorkspaceId,
    pub floating: bool,
    pub position: Option<AppRulePosition>,
    pub size: Option<AppRuleSize>,
    pub focus: bool,
    pub prev_rule_decision: bool,
}

impl AppRuleEffects {
    pub fn should_float(self, was_floating: bool) -> bool {
        self.floating || (!self.prev_rule_decision && was_floating)
    }

    pub fn floating_placement(
        self,
        window: WindowId,
        space: SpaceId,
    ) -> Option<AppRulePlacement> {
        (self.floating && (self.position.is_some() || self.size.is_some())).then_some(
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

    pub fn push_resize(&mut self, resize: AppRuleResize) {
        self.resizes.push(resize);
    }

    pub fn has_resizes(&self) -> bool {
        !self.resizes.is_empty()
    }

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
    pub position: Option<AppRulePosition>,
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
            let travel_x = (screen.size.width - frame.size.width).max(0.0);
            let travel_y = (screen.size.height - frame.size.height).max(0.0);
            frame.origin = CGPoint::new(
                screen.origin.x + travel_x * position.x,
                screen.origin.y + travel_y * position.y,
            );
        }
        frame
    }
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
    /// Its place in the layout changed (newly assigned, float flipped, or no longer ignored).
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
                if in_layout_now { Self::RemoveFromLayout } else { Self::Settled }
            }
            Err(()) => {
                if !before.assigned || before.ignored { Self::RefreshLayout } else { Self::Settled }
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

    fn effects(floating: bool, prev_rule_decision: bool) -> AppRuleResult {
        AppRuleResult::Managed(AppRuleEffects {
            workspace_id: VirtualWorkspaceId::default(),
            floating,
            position: None,
            size: None,
            focus: false,
            prev_rule_decision,
        })
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
        let settled = BeforeRules { assigned: true, floating: false, ignored: false };
        assert_eq!(AfterRules::for_result(settled, Ok(&effects(false, true)), true), AfterRules::Settled);
        assert_eq!(AfterRules::for_result(settled, Ok(&effects(true, true)), true), AfterRules::RefreshLayout);
        let unassigned = BeforeRules { assigned: false, ..settled };
        assert_eq!(AfterRules::for_result(unassigned, Ok(&effects(false, true)), true), AfterRules::RefreshLayout);
        let ignored = BeforeRules { ignored: true, ..settled };
        assert_eq!(AfterRules::for_result(ignored, Ok(&effects(false, true)), true), AfterRules::RefreshLayout);
    }

    #[test]
    fn a_rule_that_says_nothing_about_floating_keeps_the_windows_current_state() {
        let floating = BeforeRules { assigned: true, floating: true, ignored: false };
        assert_eq!(AfterRules::for_result(floating, Ok(&effects(false, false)), true), AfterRules::Settled);
        assert_eq!(AfterRules::for_result(floating, Ok(&effects(false, true)), true), AfterRules::RefreshLayout);
    }

    #[test]
    fn an_unmanaged_verdict_removes_only_what_the_layout_still_holds() {
        let before = BeforeRules { assigned: true, floating: false, ignored: false };
        assert_eq!(AfterRules::for_result(before, Ok(&AppRuleResult::Unmanaged), true), AfterRules::RemoveFromLayout);
        assert_eq!(AfterRules::for_result(before, Ok(&AppRuleResult::Unmanaged), false), AfterRules::Settled);
    }

    #[test]
    fn a_failed_evaluation_refreshes_a_window_the_layout_did_not_have() {
        let held = BeforeRules { assigned: true, floating: false, ignored: false };
        assert_eq!(AfterRules::for_result(held, Err(()), true), AfterRules::Settled);
        assert_eq!(AfterRules::for_result(BeforeRules { assigned: false, ..held }, Err(()), true), AfterRules::RefreshLayout);
        assert_eq!(AfterRules::for_result(BeforeRules { ignored: true, ..held }, Err(()), true), AfterRules::RefreshLayout);
    }
}
