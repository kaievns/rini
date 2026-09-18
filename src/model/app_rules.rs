use objc2_core_foundation::{CGPoint, CGRect};
use regex::{Regex, RegexBuilder};
use tracing::warn;

use crate::actor::app::WindowId;
use crate::common::config::{AppRulePosition, AppRuleSize, AppWorkspaceRule, WorkspaceSelector};
use crate::model::VirtualWorkspaceId;
use crate::sys::screen::SpaceId;

#[derive(Debug, Clone, Copy, Default)]
pub struct WindowRuleContext<'a> {
    pub app_bundle_id: Option<&'a str>,
    pub app_name: Option<&'a str>,
    pub window_title: Option<&'a str>,
    pub ax_role: Option<&'a str>,
    pub ax_subrole: Option<&'a str>,
    /// The window's `AXModal` attribute.
    pub is_modal: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppRuleDecision {
    NoMatch,
    Unmanaged,
    Managed {
        workspace: Option<WorkspaceSelector>,
        floating: bool,
        position: Option<AppRulePosition>,
        size: Option<AppRuleSize>,
        focus: bool,
    },
}

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
    pub(crate) fn should_float(self, was_floating: bool) -> bool {
        self.floating || (!self.prev_rule_decision && was_floating)
    }

    pub(crate) fn floating_placement(
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

    pub(crate) fn tiled_resize(
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
pub(crate) struct AppRuleOutcome {
    placements: Vec<AppRulePlacement>,
    resizes: Vec<AppRuleResize>,
    workspace_focus: Option<AppRuleWorkspaceFocus>,
}

impl AppRuleOutcome {
    pub(crate) fn push_placement(&mut self, placement: AppRulePlacement) {
        self.placements.push(placement);
    }

    pub(crate) fn push_resize(&mut self, resize: AppRuleResize) {
        self.resizes.push(resize);
    }

    pub(crate) fn has_resizes(&self) -> bool {
        !self.resizes.is_empty()
    }

    pub(crate) fn set_workspace_focus(&mut self, focus: AppRuleWorkspaceFocus) {
        self.workspace_focus = Some(focus);
    }

    pub(crate) fn into_parts(
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
pub(crate) struct AppRulePlacement {
    pub(crate) window: WindowId,
    pub(crate) space: SpaceId,
    pub(crate) position: Option<AppRulePosition>,
    pub(crate) size: Option<AppRuleSize>,
}

impl AppRulePlacement {
    pub(crate) fn resolve_frame(self, current: CGRect, screen: CGRect) -> CGRect {
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
pub(crate) struct AppRuleResize {
    pub(crate) window: WindowId,
    pub(crate) space: SpaceId,
    pub(crate) workspace_id: VirtualWorkspaceId,
    pub(crate) size: AppRuleSize,
}

/// Reactor-owned part of a focus rule: switching workspaces requires saving
/// the currently visible floating frames before the engine activates the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppRuleWorkspaceFocus {
    pub(crate) window: WindowId,
    pub(crate) space: SpaceId,
    pub(crate) workspace_index: usize,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    rule: AppWorkspaceRule,
    title_regex: Option<Regex>,
}

/// Compiles and evaluates app policy without depending on workspaces, windows,
/// the reactor, or the layout engine.
#[derive(Debug, Clone, Default)]
pub struct AppRuleEngine {
    rules: Vec<CompiledRule>,
    /// `float_modal_windows`: a modal window floats unless the rule that matched it names `modal`.
    float_modals: bool,
}

impl AppRuleEngine {
    pub fn new(rules: &[AppWorkspaceRule], float_modals: bool) -> Self {
        let rules = rules
            .iter()
            .cloned()
            .map(|rule| {
                let title_regex =
                    rule.title_regex.as_deref().filter(|value| !value.is_empty()).and_then(
                        |value| {
                            RegexBuilder::new(value)
                            .case_insensitive(true)
                            .build()
                            .map_err(|error| {
                                warn!(%error, pattern = value, "invalid title regex in app rule");
                            })
                            .ok()
                        },
                    );
                CompiledRule { rule, title_regex }
            })
            .collect();
        Self { rules, float_modals }
    }

    /// The best matching rule's decision. A modal window floats by default: with no rule, or
    /// with a rule that does not name `modal` (an `app_id` rule sending an app to a workspace
    /// should not tile that app's dialogs). A rule naming `modal` decides for itself.
    pub fn evaluate(&self, context: WindowRuleContext<'_>) -> AppRuleDecision {
        let best = self
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule.matches(context))
            .max_by_key(|(index, rule)| (rule.specificity(), std::cmp::Reverse(*index)));
        let modal_floats = self.float_modals && context.is_modal;
        let Some((_, matched)) = best else {
            return if modal_floats {
                AppRuleDecision::Managed {
                    workspace: None,
                    floating: true,
                    position: None,
                    size: None,
                    focus: false,
                }
            } else {
                AppRuleDecision::NoMatch
            };
        };
        if !matched.rule.manage {
            AppRuleDecision::Unmanaged
        } else {
            AppRuleDecision::Managed {
                workspace: matched.rule.workspace.clone(),
                floating: matched.rule.floating || (modal_floats && matched.rule.modal.is_none()),
                position: matched.rule.position,
                size: matched.rule.size,
                focus: matched.rule.focus,
            }
        }
    }
}

impl CompiledRule {
    fn matches(&self, context: WindowRuleContext<'_>) -> bool {
        optional_eq_ignore_case(self.rule.app_id.as_deref(), context.app_bundle_id)
            && optional_fuzzy_name(self.rule.app_name.as_deref(), context.app_name)
            && optional_regex(
                self.rule.title_regex.as_deref(),
                self.title_regex.as_ref(),
                context.window_title,
            )
            && optional_contains(self.rule.title_substring.as_deref(), context.window_title)
            && optional_exact(self.rule.ax_role.as_deref(), context.ax_role)
            && optional_exact(self.rule.ax_subrole.as_deref(), context.ax_subrole)
            && self.rule.modal.is_none_or(|modal| modal == context.is_modal)
    }

    fn specificity(&self) -> usize {
        [
            self.rule.app_id.as_deref(),
            self.rule.app_name.as_deref(),
            self.rule.title_regex.as_deref(),
            self.rule.title_substring.as_deref(),
            self.rule.ax_role.as_deref(),
            self.rule.ax_subrole.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .count()
            + usize::from(self.rule.modal.is_some())
    }
}

fn optional_eq_ignore_case(rule: Option<&str>, actual: Option<&str>) -> bool {
    rule.is_none_or(|rule| actual.is_some_and(|actual| rule.eq_ignore_ascii_case(actual)))
}
fn optional_fuzzy_name(rule: Option<&str>, actual: Option<&str>) -> bool {
    rule.is_none_or(|rule| {
        actual.is_some_and(|actual| {
            let (rule, actual) = (rule.to_lowercase(), actual.to_lowercase());
            rule.contains(&actual) || actual.contains(&rule)
        })
    })
}
fn optional_regex(pattern: Option<&str>, regex: Option<&Regex>, actual: Option<&str>) -> bool {
    pattern.is_none_or(|pattern| {
        !pattern.is_empty()
            && regex.is_some_and(|regex| actual.is_some_and(|actual| regex.is_match(actual)))
    })
}
fn optional_contains(rule: Option<&str>, actual: Option<&str>) -> bool {
    rule.is_none_or(|rule| {
        !rule.is_empty()
            && actual.is_some_and(|actual| actual.to_lowercase().contains(&rule.to_lowercase()))
    })
}
fn optional_exact(rule: Option<&str>, actual: Option<&str>) -> bool {
    rule.is_none_or(|rule| !rule.is_empty() && actual == Some(rule))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_without_workspace_or_layout_state() {
        let rule = AppWorkspaceRule {
            app_id: Some("com.example.Editor".into()),
            workspace: None,
            floating: true,
            position: Some(AppRulePosition { x: 0.4, y: 0.7 }),
            size: Some(AppRuleSize { w: Some(640.0), h: Some(480.0) }),
            focus: true,
            manage: true,
            app_name: None,
            title_regex: Some("project \\d+".into()),
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
            modal: None,
        };
        let engine = AppRuleEngine::new(&[rule], true);
        assert_eq!(
            engine.evaluate(WindowRuleContext {
                app_bundle_id: Some("COM.EXAMPLE.EDITOR"),
                window_title: Some("Project 42"),
                ..Default::default()
            }),
            AppRuleDecision::Managed {
                workspace: None,
                floating: true,
                position: Some(AppRulePosition { x: 0.4, y: 0.7 }),
                size: Some(AppRuleSize { w: Some(640.0), h: Some(480.0) }),
                focus: true,
            }
        );
    }

    fn rule(app_id: Option<&str>, modal: Option<bool>, floating: bool) -> AppWorkspaceRule {
        AppWorkspaceRule {
            app_id: app_id.map(Into::into),
            workspace: Some(WorkspaceSelector::Index(2)),
            floating,
            position: None,
            size: None,
            focus: false,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
            modal,
        }
    }

    fn floats(decision: &AppRuleDecision) -> Option<bool> {
        match decision {
            AppRuleDecision::Managed { floating, .. } => Some(*floating),
            _ => None,
        }
    }

    /// `float_modal_windows`: a modal floats with no rule, and under a rule that does not name
    /// `modal` (the rule's other effects, such as the workspace, still apply). A rule naming
    /// `modal` decides for itself; with the setting off a modal is any other window.
    #[test]
    fn a_modal_window_floats_unless_a_rule_naming_modal_says_otherwise() {
        let modal = WindowRuleContext {
            app_bundle_id: Some("com.example.Editor"),
            is_modal: true,
            ..Default::default()
        };
        let plain = WindowRuleContext { is_modal: false, ..modal };

        let none = AppRuleEngine::new(&[], true);
        assert_eq!(floats(&none.evaluate(modal)), Some(true));
        assert_eq!(none.evaluate(plain), AppRuleDecision::NoMatch);

        let by_app = AppRuleEngine::new(&[rule(Some("com.example.Editor"), None, false)], true);
        let decision = by_app.evaluate(modal);
        assert_eq!(floats(&decision), Some(true), "an app rule does not tile the app's dialogs");
        assert!(matches!(decision, AppRuleDecision::Managed { workspace: Some(_), .. }), "{decision:?}");
        assert_eq!(floats(&by_app.evaluate(plain)), Some(false));

        let tiled_modals = AppRuleEngine::new(&[rule(Some("com.example.Editor"), Some(true), false)], true);
        assert_eq!(floats(&tiled_modals.evaluate(modal)), Some(false), "a rule naming modal wins");
        assert_eq!(tiled_modals.evaluate(plain), AppRuleDecision::NoMatch, "modal = true does not match a plain window");

        let off = AppRuleEngine::new(&[], false);
        assert_eq!(off.evaluate(modal), AppRuleDecision::NoMatch);
    }

    /// `modal` counts toward specificity like every other match field: a rule naming it beats a
    /// bare `app_id` rule for the same app.
    #[test]
    fn a_rule_naming_modal_is_more_specific_than_one_that_does_not() {
        let engine = AppRuleEngine::new(
            &[rule(Some("com.example.Editor"), None, false), rule(Some("com.example.Editor"), Some(true), false)],
            true,
        );
        let modal = WindowRuleContext {
            app_bundle_id: Some("com.example.Editor"),
            is_modal: true,
            ..Default::default()
        };
        assert_eq!(floats(&engine.evaluate(modal)), Some(false));
    }
}
