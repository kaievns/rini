//! What a key binding can name: a wire command, or the few things only a key can ask for
//! (`exec`, `reload_config`, a workspace by name). Plain data; the application interprets it.

use std::borrow::Cow;

use once_cell::sync::Lazy;
pub use rini_ipc::protocol::Command;
use rini_ipc::protocol::WorkspaceSelector;
use serde::{Deserialize, Serialize};
use strum::VariantNames;


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum WmCommand {
    Wm(WmCmd),
    ReactorCommand(Command),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, strum_macros::VariantNames)]
#[serde(rename_all = "snake_case")]
pub enum WmCmd {
    ToggleSpaceActivated,
    Exec(ExecCmd),
    ReloadConfig,

    NextWorkspace,
    PrevWorkspace,
    SwitchToWorkspace(WorkspaceSelector),
    MoveWindowToWorkspace(WorkspaceSelector),
    CreateWorkspace,
    SwitchToLastWorkspace,

    CloseWindow,

    /// Cycle the focused app's windows across workspaces and displays.
    ///
    /// Two unit variants rather than one taking `{ backward: bool }`. `WmCommand` is
    /// `#[serde(untagged)]`, so a struct-bodied variant whose only field has a default
    /// cannot be written as a bare string in a keybinding: `"cycle_app_windows"` matched
    /// neither arm, and rini PANICS at startup on an unparseable binding rather than
    /// skipping it, so the whole WM failed to start. Unit variants keep both directions
    /// expressible as plain strings.
    CycleAppWindows,
    CycleAppWindowsBackward,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ExecCmd {
    String(String),
    Array(Vec<String>),
}

static BUILTIN_WM_CMD_VARIANTS: Lazy<Vec<String>> = Lazy::new(|| {
    WmCmd::VARIANTS
        .iter()
        .map(|v| {
            let mut out = String::with_capacity(v.len());
            for (i, ch) in v.chars().enumerate() {
                if ch.is_uppercase() {
                    if i != 0 {
                        out.push('_');
                    }
                    for lc in ch.to_lowercase() {
                        out.push(lc);
                    }
                } else {
                    out.push(ch);
                }
            }
            out
        })
        .collect()
});

impl WmCmd {
    pub fn snake_case_variants() -> &'static [String] {
        &BUILTIN_WM_CMD_VARIANTS
    }
}

impl WmCommand {
    pub fn builtin_candidates() -> &'static [String] {
        WmCmd::snake_case_variants()
    }
}

impl ExecCmd {
    /// The argv to run. The string form splits on single spaces only; quoting is not
    /// interpreted, so use the array form for arguments that contain spaces.
    pub fn as_array(&self) -> Cow<'_, [String]> {
        match self {
            ExecCmd::Array(vec) => Cow::Borrowed(vec),
            ExecCmd::String(s) => s.split(' ').map(|s| s.to_owned()).collect::<Vec<_>>().into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rini_ipc::protocol::LayoutCommand;

    #[test]
    fn every_builtin_candidate_is_the_name_the_config_accepts() {
        let candidates = WmCommand::builtin_candidates();
        assert_eq!(candidates.len(), WmCmd::VARIANTS.len());
        for name in candidates {
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name} is not snake_case"
            );
            let bare = serde_json::Value::String(name.clone());
            let with_payload = serde_json::json!({ name: 1 });
            let parses = serde_json::from_value::<WmCommand>(bare).is_ok()
                || serde_json::from_value::<WmCommand>(with_payload).is_ok()
                || serde_json::from_value::<WmCommand>(serde_json::json!({ name: "x" })).is_ok();
            assert!(parses, "{name} is suggested but the config does not accept it");
        }
        assert!(candidates.iter().any(|c| c == "cycle_app_windows_backward"));
    }

    #[test]
    fn a_bare_string_that_is_not_a_wm_command_falls_through_to_the_reactor_commands() {
        let parsed: WmCommand = serde_json::from_str(r#""next_window""#).unwrap();
        assert_eq!(parsed, WmCommand::ReactorCommand(Command::Layout(LayoutCommand::NextWindow)));
        assert!(serde_json::from_str::<WmCommand>(r#""no_such_command""#).is_err());
    }

    #[test]
    fn exec_string_form_splits_on_spaces_without_quote_handling() {
        let cmd: ExecCmd = serde_json::from_str(r#""open -a 'My App'""#).unwrap();
        assert_eq!(cmd.as_array().as_ref(), ["open", "-a", "'My", "App'"]);
        let cmd: ExecCmd = serde_json::from_str(r#"["open", "-a", "My App"]"#).unwrap();
        assert_eq!(cmd.as_array().as_ref(), ["open", "-a", "My App"]);
    }
}
