//! What a key binding can name: rini's actions, in the config's own words. Plain data; the
//! application lowers each to a wire command or does it itself.
//!
//! The names are niri's where rini does what niri does, and follow niri's grammar where it does not
//! (`docs/config.md` has the table). Directions are part of the name, as niri's are.

use std::str::FromStr;

use knus::ast::{Literal, TypeName};
use knus::decode::{Context, Kind};
use knus::errors::DecodeError;
use knus::span::Spanned;
use knus::traits::ErrorSpan;
pub use rini_ipc::protocol::{RestoreScope, RestoreSource, SwitchScope};
use serde::Serialize;

use crate::input::domain::key::{ModKey, substitute_mod};

/// One `binds` entry.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Bind {
    /// The key as written, canonicalised, with `Mod` still standing for `input › mod-key`.
    pub key: String,
    pub action: Action,
    /// Fire again while the key is held. Off unless asked for, since a repeat re-fires commands such
    /// as workspace back-and-forth on every one.
    pub repeat: bool,
}

impl Bind {
    /// The key with `Mod` replaced by the modifier it stands for.
    pub fn resolved_key(&self, mod_key: ModKey) -> String { substitute_mod(&self.key, mod_key) }
}

#[derive(knus::Decode, strum::VariantNames, Serialize, Debug, Clone, PartialEq)]
#[strum(serialize_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    FocusColumnLeft,
    FocusColumnRight,
    /// Within the column, then the display below at its end.
    FocusWindowOrMonitorDown,
    FocusWindowOrMonitorUp,
    /// A lone window swaps columns; a stacked one leaves as a new column.
    MoveWindowLeft,
    MoveWindowRight,
    MoveWindowDownOrToMonitorDown,
    MoveWindowUpOrToMonitorUp,
    ToggleFoldLeft,
    ToggleFoldRight,
    ToggleStack,
    /// The windows on screen, wrapping.
    CycleVisibleWindows,
    CycleVisibleWindowsBack,
    ToggleWindowFloating,
    SwitchFocusBetweenFloatingAndTiling,
    MaximizeColumn,
    SwitchPresetColumnWidth,
    CenterColumn,
    SetColumnWidth(#[knus(argument, str)] SizeChange),
    SetWindowHeight(#[knus(argument, str)] SizeChange),
    SetColumnWidthOrWindowHeight(#[knus(argument, str)] SizeChange),
    SnapStrip,
    FocusWorkspace(#[knus(argument)] WorkspaceReference),
    FocusWorkspaceDown(#[knus(property(name = "skip-empty"))] Option<bool>),
    FocusWorkspaceUp(#[knus(property(name = "skip-empty"))] Option<bool>),
    FocusWorkspacePrevious,
    MoveWindowToWorkspace(
        #[knus(argument)] WorkspaceReference,
        #[knus(property(name = "focus"), default = true)] bool,
    ),
    MoveWindowToWorkspaceDown(#[knus(property(name = "focus"), default = true)] bool),
    MoveWindowToWorkspaceUp(#[knus(property(name = "focus"), default = true)] bool),
    FocusMonitorLeft,
    FocusMonitorRight,
    FocusMonitorUp,
    FocusMonitorDown,
    /// The next display in physical order, wrapping.
    FocusMonitorNext,
    FocusMonitor(#[knus(argument)] MonitorReference),
    MoveWindowToMonitorLeft,
    MoveWindowToMonitorRight,
    MoveWindowToMonitorUp,
    MoveWindowToMonitorDown,
    MoveWindowToMonitorNext,
    MoveWindowToMonitor(#[knus(argument)] MonitorReference),
    MoveMouseToMonitorLeft,
    MoveMouseToMonitorRight,
    MoveMouseToMonitorUp,
    MoveMouseToMonitorDown,
    MoveMouseToMonitorNext,
    MoveMouseToMonitor(#[knus(argument)] MonitorReference),
    /// The native macOS Space.
    SwitchSpaceLeft,
    SwitchSpaceRight,
    ToggleSpaceActivated,
    CloseWindow,
    /// A program and its arguments, one string each, interpreting nothing.
    Spawn(#[knus(arguments)] Vec<String>),
    /// One command line, through `sh -c`.
    SpawnSh(#[knus(argument)] String),
    LoadConfigFile,
    /// Save the layout and exit.
    Quit,
    SaveLayout(#[knus(argument)] String),
    RestoreLayout {
        #[knus(argument)]
        path: String,
        #[knus(property, str)]
        scope: RestoreScope,
        #[knus(property, str)]
        source: Option<RestoreSource>,
    },
    RedistributeWindows,
    /// The switcher, only in `recent-windows › binds`.
    NextWindow {
        #[knus(property, str)]
        scope: Option<SwitcherScope>,
        #[knus(property, str)]
        filter: Option<SwitcherFilter>,
    },
    PreviousWindow {
        #[knus(property, str)]
        scope: Option<SwitcherScope>,
        #[knus(property, str)]
        filter: Option<SwitcherFilter>,
    },
}

impl Action {
    /// Whether this is one of the switcher's two actions, which belong in `recent-windows › binds`.
    pub fn is_switcher(&self) -> bool {
        matches!(self, Action::NextWindow { .. } | Action::PreviousWindow { .. })
    }

    /// The workspace this names by name, which has to be declared.
    pub fn workspace_name(&self) -> Option<&str> {
        match self {
            Action::FocusWorkspace(WorkspaceReference::Name(name))
            | Action::MoveWindowToWorkspace(WorkspaceReference::Name(name), _) => Some(name),
            _ => None,
        }
    }
}

/// The wire's scope for a switcher written `scope=` and `filter=`. Both at once is refused when the
/// config is read.
pub fn switch_scope(scope: Option<SwitcherScope>, filter: Option<SwitcherFilter>) -> SwitchScope {
    match (scope, filter) {
        (_, Some(SwitcherFilter::AppId)) => SwitchScope::App,
        (Some(SwitcherScope::Workspace), None) => SwitchScope::Workspace,
        (Some(SwitcherScope::All) | None, None) => SwitchScope::Everything,
    }
}

/// A number written as an integer or a decimal.
#[derive(Serialize, Debug, Clone, Copy, PartialEq)]
#[serde(transparent)]
pub struct Decimal(pub f64);

impl<S: ErrorSpan> knus::DecodeScalar<S> for Decimal {
    fn type_check(type_name: &Option<Spanned<TypeName, S>>, ctx: &mut Context<S>) {
        if let Some(type_name) = type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected",
            ));
        }
    }

    fn raw_decode(value: &Spanned<Literal, S>, _: &mut Context<S>) -> Result<Self, DecodeError<S>> {
        let parsed = match &**value {
            Literal::Int(int) => i64::try_from(int).map(|n| n as f64).map_err(|e| e.to_string()),
            Literal::Decimal(decimal) => f64::try_from(decimal).map_err(|e| e.to_string()),
            _ => return Err(DecodeError::scalar_kind(Kind::Decimal, value)),
        };
        parsed.map(Decimal).map_err(|message| DecodeError::conversion(value, message))
    }
}

/// A workspace: an integer is its position, 1 being the first; a string is its name.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum WorkspaceReference {
    Index(usize),
    Name(String),
}

/// A display: an integer is its position in physical order, 1 being the first; a string its UUID.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum MonitorReference {
    Index(usize),
    Uuid(String),
}

fn decode_reference<S: ErrorSpan>(
    value: &Spanned<Literal, S>,
    what: &str,
) -> Result<Result<usize, String>, DecodeError<S>> {
    match &**value {
        Literal::Int(int) => match u64::try_from(int) {
            Ok(n) if n >= 1 => Ok(Ok(n as usize)),
            _ => Err(DecodeError::conversion(
                value,
                format!("{what} numbers start at 1, the first; 0 is not one"),
            )),
        },
        Literal::String(name) => Ok(Err(name.to_string())),
        _ => Err(DecodeError::scalar_kind(Kind::String, value)),
    }
}

impl<S: ErrorSpan> knus::DecodeScalar<S> for WorkspaceReference {
    fn type_check(type_name: &Option<Spanned<TypeName, S>>, ctx: &mut Context<S>) {
        Decimal::type_check(type_name, ctx);
    }

    fn raw_decode(value: &Spanned<Literal, S>, _: &mut Context<S>) -> Result<Self, DecodeError<S>> {
        Ok(match decode_reference(value, "workspace")? {
            Ok(index) => WorkspaceReference::Index(index),
            Err(name) => WorkspaceReference::Name(name),
        })
    }
}

impl<S: ErrorSpan> knus::DecodeScalar<S> for MonitorReference {
    fn type_check(type_name: &Option<Spanned<TypeName, S>>, ctx: &mut Context<S>) {
        Decimal::type_check(type_name, ctx);
    }

    fn raw_decode(value: &Spanned<Literal, S>, _: &mut Context<S>) -> Result<Self, DecodeError<S>> {
        Ok(match decode_reference(value, "monitor")? {
            Ok(index) => MonitorReference::Index(index),
            Err(uuid) => MonitorReference::Uuid(uuid),
        })
    }
}

/// A relative change of a proportion: `"+5%"` or `"-5%"`.
#[derive(Serialize, Debug, Clone, Copy, PartialEq)]
#[serde(transparent)]
pub struct SizeChange(pub f64);

impl FromStr for SizeChange {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let not_yet = || {
            format!(
                "`{text}`: rini takes a relative percentage, \"+5%\" or \"-5%\"; an absolute \
                 percentage or a size in pixels is not supported yet"
            )
        };
        let (sign, rest) = match text.as_bytes().first() {
            Some(b'+') => (1.0, &text[1..]),
            Some(b'-') => (-1.0, &text[1..]),
            _ => return Err(not_yet()),
        };
        let number = rest.strip_suffix('%').ok_or_else(not_yet)?;
        let value: f64 = number.parse().map_err(|_| not_yet())?;
        Ok(SizeChange(sign * value / 100.0))
    }
}

/// Which windows the switcher steps through.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SwitcherScope {
    All,
    Workspace,
}

impl FromStr for SwitcherScope {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "all" => Ok(SwitcherScope::All),
            "workspace" => Ok(SwitcherScope::Workspace),
            "output" => Err(
                "rini's workspaces span every display, so there is no switcher for \
                             one display; use scope=\"workspace\""
                    .to_owned(),
            ),
            other => Err(format!("expected `all` or `workspace`, found `{other}`")),
        }
    }
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SwitcherFilter {
    /// The focused application's windows, most recently focused first.
    AppId,
}

impl FromStr for SwitcherFilter {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "app-id" => Ok(SwitcherFilter::AppId),
            other => Err(format!("expected `app-id`, found `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use strum::VariantNames;

    use super::*;

    #[test]
    fn a_relative_percentage_is_a_proportion() {
        assert_eq!("+5%".parse::<SizeChange>(), Ok(SizeChange(0.05)));
        assert_eq!("-10%".parse::<SizeChange>(), Ok(SizeChange(-0.1)));
        for not_yet in ["5%", "+10", "640", "+5", "-", "+x%"] {
            assert!(not_yet.parse::<SizeChange>().is_err(), "{not_yet}");
        }
    }

    #[test]
    fn every_action_name_is_kebab_case_and_counted() {
        assert_eq!(Action::VARIANTS.len(), 60);
        for name in Action::VARIANTS {
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{name} is not kebab-case"
            );
        }
    }

    #[test]
    fn only_the_two_switcher_actions_are_switcher_actions() {
        let next = Action::NextWindow { scope: None, filter: None };
        let previous = Action::PreviousWindow { scope: None, filter: None };
        assert!(next.is_switcher() && previous.is_switcher());
        assert!(!Action::FocusColumnLeft.is_switcher());
    }

    #[test]
    fn only_a_workspace_named_by_name_is_a_workspace_name() {
        let named = |name: &str| WorkspaceReference::Name(name.into());
        assert_eq!(Action::FocusWorkspace(named("mail")).workspace_name(), Some("mail"));
        assert_eq!(
            Action::MoveWindowToWorkspace(named("code"), true).workspace_name(),
            Some("code")
        );
        assert_eq!(
            Action::FocusWorkspace(WorkspaceReference::Index(2)).workspace_name(),
            None
        );
        assert_eq!(Action::FocusWorkspaceUp(None).workspace_name(), None);
    }

    #[test]
    fn a_switcher_scoped_to_one_display_is_refused_with_the_reason() {
        let error = "output".parse::<SwitcherScope>().unwrap_err();
        assert!(error.contains("span every display"), "{error}");
        assert_eq!("workspace".parse(), Ok(SwitcherScope::Workspace));
    }

    #[test]
    fn mod_resolves_to_the_mod_key() {
        let bind = Bind {
            key: "Mod+Shift+J".into(),
            action: Action::MoveWindowLeft,
            repeat: false,
        };
        assert_eq!(bind.resolved_key(ModKey::Ctrl), "Ctrl+Shift+J");
        assert_eq!(bind.resolved_key(ModKey::Alt), "Alt+Shift+J");
    }
}
