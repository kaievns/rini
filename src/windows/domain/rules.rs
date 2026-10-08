//! Window rules: which windows rini manages, and what the rules that match a window ask for it.
//! Matching only; resolving the result against workspaces is `crate::workspaces::domain::app_rules`.
//!
//! niri's semantics. A rule applies to a window that matches ANY of its `match` lines and NONE of
//! its `exclude` lines, and a rule with no `match` applies to every window. Every rule that applies
//! is applied, in order, a later one overriding an earlier one property by property. One matcher
//! (`applies`) serves both folds: the one here, at open, and what a window wears
//! (`crate::decorations::domain::rules`), whenever its state changes.
use std::fmt;
use std::str::FromStr;

use regex::Regex;
pub use rini_ipc::protocol::WorkspaceSelector;
use serde::{Serialize, Serializer};

use crate::workspaces::domain::display_pin::DisplayRole;

/// A regular expression that compares by its pattern, so a rule can be compared and printed.
#[derive(Clone)]
pub struct RegexEq(Regex);

impl RegexEq {
    pub fn is_match(&self, text: &str) -> bool { self.0.is_match(text) }

    pub fn as_str(&self) -> &str { self.0.as_str() }
}

impl FromStr for RegexEq {
    type Err = regex::Error;

    fn from_str(pattern: &str) -> Result<Self, Self::Err> { Regex::new(pattern).map(Self) }
}

impl PartialEq for RegexEq {
    fn eq(&self, other: &Self) -> bool { self.as_str() == other.as_str() }
}

impl fmt::Debug for RegexEq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{:?}", self.as_str()) }
}

impl Serialize for RegexEq {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One `match` or `exclude` line: every matcher it names must hold.
#[derive(Serialize, Debug, PartialEq, Clone, Default)]
pub struct RuleMatch {
    /// The bundle id.
    pub app_id: Option<RegexEq>,
    pub title: Option<RegexEq>,
    /// The application's localised name.
    pub app_name: Option<RegexEq>,
    /// Exact `AXRole`.
    pub ax_role: Option<String>,
    /// Exact `AXSubrole`.
    pub ax_subrole: Option<String>,
    /// The window's `AXModal` attribute.
    pub is_modal: Option<bool>,
    /// Whether the window wears the active paint. rini draws one such window, the one holding focus,
    /// so this and `is_focused` match the same window.
    pub is_active: Option<bool>,
    /// Whether the window holds focus.
    pub is_focused: Option<bool>,
    pub is_floating: Option<bool>,
    /// Whether the window is drawn urgent: its application asks for attention and holds no focus.
    pub is_urgent: Option<bool>,
}

impl RuleMatch {
    fn holds(&self, context: WindowRuleContext<'_>) -> bool {
        let state = context.state;
        regex_holds(self.app_id.as_ref(), context.app_bundle_id)
            && regex_holds(self.title.as_ref(), context.window_title)
            && regex_holds(self.app_name.as_ref(), context.app_name)
            && exact_holds(self.ax_role.as_deref(), context.ax_role)
            && exact_holds(self.ax_subrole.as_deref(), context.ax_subrole)
            && self.is_modal.is_none_or(|modal| modal == context.is_modal)
            && self.is_active.is_none_or(|active| active == state.focused)
            && self.is_focused.is_none_or(|focused| focused == state.focused)
            && self.is_floating.is_none_or(|floating| floating == state.floating)
            && self.is_urgent.is_none_or(|urgent| urgent == state.urgent)
    }

    /// Whether it names a matcher that turns as focus moves or an application asks for attention.
    pub fn follows_focus_or_attention(&self) -> bool {
        self.is_active.is_some() || self.is_focused.is_some() || self.is_urgent.is_some()
    }
}

/// Whether a rule with these lines applies to `context`'s window: it matches one of `matches`, or
/// there are none, and none of `excludes`.
pub fn applies(matches: &[RuleMatch], excludes: &[RuleMatch], context: WindowRuleContext<'_>) -> bool {
    (matches.is_empty() || matches.iter().any(|m| m.holds(context))) && !excludes.iter().any(|m| m.holds(context))
}

fn regex_holds(regex: Option<&RegexEq>, actual: Option<&str>) -> bool {
    regex.is_none_or(|regex| actual.is_some_and(|actual| regex.is_match(actual)))
}

fn exact_holds(rule: Option<&str>, actual: Option<&str>) -> bool {
    rule.is_none_or(|rule| actual == Some(rule))
}

/// Which corner or side of the display a floating position is measured from.
#[derive(Serialize, Debug, PartialEq, Eq, Clone, Copy, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RelativeTo {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Top,
    Bottom,
    Left,
    Right,
}

/// Where a floating window first appears: logical pixels from a corner, or from the middle of a side.
#[derive(Serialize, Debug, PartialEq, Clone, Copy, Default)]
pub struct FloatingPosition {
    pub x: f64,
    pub y: f64,
    pub relative_to: RelativeTo,
}

/// A window's first size in logical pixels, either dimension or both.
#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
pub struct AppRuleSize {
    pub w: Option<f64>,
    pub h: Option<f64>,
}

/// One `window-rule`. Every property is optional, so a later rule can leave what an earlier one said.
#[derive(Serialize, Debug, PartialEq, Clone, Default)]
pub struct WindowRule {
    pub matches: Vec<RuleMatch>,
    pub excludes: Vec<RuleMatch>,
    /// A workspace by name.
    pub open_on_workspace: Option<String>,
    pub open_floating: Option<bool>,
    pub default_floating_position: Option<FloatingPosition>,
    /// Logical pixels.
    pub default_column_width: Option<f64>,
    pub default_window_height: Option<f64>,
    /// Focus the window when it opens, switching to its workspace.
    pub open_focused: Option<bool>,
    /// `false`: rini leaves the window alone completely.
    pub manage: Option<bool>,
    /// Keep matching windows on the internal or the external display.
    ///
    /// A ROLE, not a display: the external at home and the one at the office have different UUIDs,
    /// and a rule naming one would be silently inert at the other desk. A default rather than a law,
    /// so an explicit move still wins, and a role nothing fills leaves the window where it can go.
    pub open_on_output: Option<DisplayRole>,
}

impl WindowRule {
    fn applies(&self, context: WindowRuleContext<'_>) -> bool { applies(&self.matches, &self.excludes, context) }

    /// Whether it sets anything a window opens with.
    pub fn sets_anything(&self) -> bool {
        *self != WindowRule { matches: self.matches.clone(), excludes: self.excludes.clone(), ..WindowRule::default() }
    }

    /// `self` with every property `later` sets replaced by `later`'s.
    fn overlay(&mut self, later: &WindowRule) {
        let later = later.clone();
        self.open_on_workspace = later.open_on_workspace.or(self.open_on_workspace.take());
        self.open_floating = later.open_floating.or(self.open_floating);
        self.default_floating_position =
            later.default_floating_position.or(self.default_floating_position);
        self.default_column_width = later.default_column_width.or(self.default_column_width);
        self.default_window_height = later.default_window_height.or(self.default_window_height);
        self.open_focused = later.open_focused.or(self.open_focused);
        self.manage = later.manage.or(self.manage);
        self.open_on_output = later.open_on_output.or(self.open_on_output);
    }
}

#[cfg(test)]
impl RuleMatch {
    /// A bundle id, matched exactly.
    pub fn app(bundle_id: &str) -> Self {
        let pattern = format!("^{}$", regex::escape(bundle_id));
        Self {
            app_id: Some(pattern.parse().expect("an escaped literal")),
            ..Self::default()
        }
    }

    pub fn title(pattern: &str) -> Self {
        Self {
            title: Some(pattern.parse().expect("a valid pattern")),
            ..Self::default()
        }
    }
}

#[cfg(test)]
impl WindowRule {
    /// A rule with one `match` line and no properties yet.
    pub fn when(line: RuleMatch) -> Self {
        Self {
            matches: vec![line],
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WindowRuleContext<'a> {
    pub app_bundle_id: Option<&'a str>,
    pub app_name: Option<&'a str>,
    pub window_title: Option<&'a str>,
    pub ax_role: Option<&'a str>,
    pub ax_subrole: Option<&'a str>,
    /// The window's `AXModal` attribute.
    pub is_modal: bool,
    pub state: RuleState,
}

/// What the matchers that change while a window is open read. `Default` is a window as it opens:
/// not focused, not floating and not urgent, as niri reads one, so a rule that asks for any of them
/// sets nothing at open.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RuleState {
    pub focused: bool,
    pub floating: bool,
    pub urgent: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppRuleDecision {
    NoMatch,
    Unmanaged,
    Managed {
        workspace: Option<WorkspaceSelector>,
        floating: bool,
        position: Option<FloatingPosition>,
        size: Option<AppRuleSize>,
        focus: bool,
        /// The display role this window is pinned to, if a rule names one.
        display: Option<DisplayRole>,
    },
}

/// Evaluates the window rules without depending on workspaces, windows, the reactor, or the layout
/// engine.
#[derive(Debug, Clone, Default)]
pub struct AppRuleEngine {
    rules: Vec<WindowRule>,
}

impl AppRuleEngine {
    pub fn new(rules: &[WindowRule]) -> Self { Self { rules: rules.to_vec() } }

    /// Every rule that applies, folded in order into one decision.
    pub fn evaluate(&self, context: WindowRuleContext<'_>) -> AppRuleDecision {
        let mut folded: Option<WindowRule> = None;
        for rule in self.rules.iter().filter(|rule| rule.applies(context)) {
            folded.get_or_insert_with(WindowRule::default).overlay(rule);
        }
        let Some(rule) = folded else {
            return AppRuleDecision::NoMatch;
        };
        if rule.manage == Some(false) {
            return AppRuleDecision::Unmanaged;
        }
        let size = (rule.default_column_width.is_some() || rule.default_window_height.is_some())
            .then_some(AppRuleSize {
                w: rule.default_column_width,
                h: rule.default_window_height,
            });
        AppRuleDecision::Managed {
            workspace: rule.open_on_workspace.map(WorkspaceSelector::Name),
            floating: rule.open_floating.unwrap_or(false),
            position: rule.default_floating_position,
            size,
            focus: rule.open_focused.unwrap_or(false),
            display: rule.open_on_output,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regex(pattern: &str) -> Option<RegexEq> { Some(pattern.parse().expect("a valid pattern")) }

    fn app(pattern: &str) -> RuleMatch {
        RuleMatch {
            app_id: regex(pattern),
            ..RuleMatch::default()
        }
    }

    fn window(app_id: &str) -> WindowRuleContext<'_> {
        WindowRuleContext {
            app_bundle_id: Some(app_id),
            ..WindowRuleContext::default()
        }
    }

    fn floats(decision: &AppRuleDecision) -> Option<bool> {
        match decision {
            AppRuleDecision::Managed { floating, .. } => Some(*floating),
            _ => None,
        }
    }

    #[test]
    fn no_rule_applying_is_no_match() {
        let engine = AppRuleEngine::new(&[WindowRule {
            matches: vec![app("^com\\.example\\.Editor$")],
            open_floating: Some(true),
            ..WindowRule::default()
        }]);
        assert_eq!(
            engine.evaluate(window("com.example.Other")),
            AppRuleDecision::NoMatch
        );
    }

    /// Every rule that applies is applied in order, a later one overriding property by property: the
    /// Outlook reminder that one rule floats and another pins gets both.
    #[test]
    fn every_applying_rule_is_folded_in_file_order() {
        let engine = AppRuleEngine::new(&[
            WindowRule {
                matches: vec![RuleMatch {
                    app_id: regex("^com\\.microsoft\\.Outlook$"),
                    title: regex("^[0-9]+ Reminders?$"),
                    ..RuleMatch::default()
                }],
                open_floating: Some(true),
                ..WindowRule::default()
            },
            WindowRule {
                matches: vec![app("^com\\.microsoft\\.Outlook$")],
                open_on_output: Some(DisplayRole::Internal),
                open_floating: Some(false),
                ..WindowRule::default()
            },
            WindowRule {
                matches: vec![RuleMatch {
                    title: regex("Reminder"),
                    ..RuleMatch::default()
                }],
                open_floating: Some(true),
                ..WindowRule::default()
            },
        ]);
        let reminder = WindowRuleContext {
            window_title: Some("2 Reminders"),
            ..window("com.microsoft.Outlook")
        };
        assert_eq!(engine.evaluate(reminder), AppRuleDecision::Managed {
            workspace: None,
            floating: true,
            position: None,
            size: None,
            focus: false,
            display: Some(DisplayRole::Internal),
        });
        let inbox = WindowRuleContext {
            window_title: Some("Inbox"),
            ..window("com.microsoft.Outlook")
        };
        assert_eq!(floats(&engine.evaluate(inbox)), Some(false));
    }

    #[test]
    fn several_match_lines_are_alternatives_and_one_line_is_a_conjunction() {
        let engine = AppRuleEngine::new(&[WindowRule {
            matches: vec![app("^com\\.agilebits\\.onepassword7$"), RuleMatch {
                app_id: regex("^com\\.apple\\.finder$"),
                ax_subrole: Some("AXDialog".into()),
                ..RuleMatch::default()
            }],
            open_floating: Some(true),
            ..WindowRule::default()
        }]);
        assert_eq!(
            floats(&engine.evaluate(window("com.agilebits.onepassword7"))),
            Some(true)
        );
        let dialog = WindowRuleContext {
            ax_subrole: Some("AXDialog"),
            ..window("com.apple.finder")
        };
        assert_eq!(floats(&engine.evaluate(dialog)), Some(true));
        assert_eq!(
            engine.evaluate(window("com.apple.finder")),
            AppRuleDecision::NoMatch
        );
    }

    #[test]
    fn an_exclude_line_stops_the_rule_applying() {
        let engine = AppRuleEngine::new(&[WindowRule {
            matches: vec![app("^us\\.zoom\\.xos$")],
            excludes: vec![RuleMatch {
                title: regex("^Zoom Meeting$"),
                ..RuleMatch::default()
            }],
            open_floating: Some(true),
            ..WindowRule::default()
        }]);
        assert_eq!(floats(&engine.evaluate(window("us.zoom.xos"))), Some(true));
        let meeting = WindowRuleContext {
            window_title: Some("Zoom Meeting"),
            ..window("us.zoom.xos")
        };
        assert_eq!(engine.evaluate(meeting), AppRuleDecision::NoMatch);
    }

    #[test]
    fn a_rule_with_no_match_line_applies_to_every_window() {
        let engine = AppRuleEngine::new(&[WindowRule {
            open_on_workspace: Some("main".into()),
            ..WindowRule::default()
        }]);
        assert!(matches!(
            engine.evaluate(window("anything")),
            AppRuleDecision::Managed { workspace: Some(WorkspaceSelector::Name(name)), .. } if name == "main"
        ));
    }

    /// Regular expressions are case-sensitive and match anywhere, as niri's are; `(?i)` and anchors
    /// say otherwise.
    #[test]
    fn matchers_are_unanchored_case_sensitive_regexes() {
        let rule = |pattern: &str| {
            AppRuleEngine::new(&[WindowRule {
                matches: vec![RuleMatch {
                    title: regex(pattern),
                    ..RuleMatch::default()
                }],
                open_floating: Some(true),
                ..WindowRule::default()
            }])
        };
        let titled = WindowRuleContext {
            window_title: Some("Project Preferences"),
            ..WindowRuleContext::default()
        };
        assert_eq!(floats(&rule("Preferences").evaluate(titled)), Some(true));
        assert_eq!(rule("preferences").evaluate(titled), AppRuleDecision::NoMatch);
        assert_eq!(floats(&rule("(?i)preferences").evaluate(titled)), Some(true));
        assert_eq!(rule("^Preferences$").evaluate(titled), AppRuleDecision::NoMatch);
    }

    /// The modal default is an ordinary rule, so a later rule decides otherwise for the windows it
    /// matches.
    #[test]
    fn a_later_rule_overrides_the_modal_rule() {
        let modal_rule = WindowRule {
            matches: vec![RuleMatch {
                is_modal: Some(true),
                ..RuleMatch::default()
            }],
            open_floating: Some(true),
            ..WindowRule::default()
        };
        let modal = WindowRuleContext {
            is_modal: true,
            ..window("com.example.Editor")
        };
        let plain = WindowRuleContext { is_modal: false, ..modal };

        let default_only = AppRuleEngine::new(std::slice::from_ref(&modal_rule));
        assert_eq!(floats(&default_only.evaluate(modal)), Some(true));
        assert_eq!(default_only.evaluate(plain), AppRuleDecision::NoMatch);

        let tiled = AppRuleEngine::new(&[modal_rule, WindowRule {
            matches: vec![RuleMatch {
                app_id: regex("^com\\.example\\.Editor$"),
                is_modal: Some(true),
                ..RuleMatch::default()
            }],
            open_floating: Some(false),
            ..WindowRule::default()
        }]);
        assert_eq!(floats(&tiled.evaluate(modal)), Some(false));
    }

    #[test]
    fn manage_false_leaves_the_window_unmanaged_and_the_sizes_become_one_size() {
        let engine = AppRuleEngine::new(&[
            WindowRule {
                matches: vec![app("^com\\.example\\.Panel$")],
                default_window_height: Some(320.0),
                ..WindowRule::default()
            },
            WindowRule {
                matches: vec![app("^com\\.example\\.Hud$")],
                manage: Some(false),
                ..WindowRule::default()
            },
        ]);
        assert!(matches!(
            engine.evaluate(window("com.example.Panel")),
            AppRuleDecision::Managed {
                size: Some(AppRuleSize { w: None, h: Some(320.0) }),
                ..
            }
        ));
        assert_eq!(
            engine.evaluate(window("com.example.Hud")),
            AppRuleDecision::Unmanaged
        );
    }
    /// At open a window holds no focus and does not float, as niri reads one: a rule asking for
    /// either sets nothing it opens with, and one asking for neither does.
    #[test]
    fn dynamic_matchers_read_the_window_as_it_opens() {
        let rule = |line: RuleMatch| {
            AppRuleEngine::new(&[WindowRule { open_on_workspace: Some("main".into()), ..WindowRule::when(line) }])
        };
        let opening = window("com.example.Editor");
        for line in [
            RuleMatch { is_active: Some(true), ..RuleMatch::default() },
            RuleMatch { is_focused: Some(true), ..RuleMatch::default() },
            RuleMatch { is_floating: Some(true), ..RuleMatch::default() },
            RuleMatch { is_urgent: Some(true), ..RuleMatch::default() },
        ] {
            assert_eq!(rule(line.clone()).evaluate(opening), AppRuleDecision::NoMatch, "{line:?}");
        }
        let unfocused = RuleMatch { is_focused: Some(false), is_floating: Some(false), ..RuleMatch::default() };
        assert_ne!(rule(unfocused.clone()).evaluate(opening), AppRuleDecision::NoMatch);
        let focused = WindowRuleContext { state: RuleState { focused: true, ..RuleState::default() }, ..opening };
        assert!(!applies(&[unfocused], &[], focused), "the same line, once the window holds focus");
    }
}
