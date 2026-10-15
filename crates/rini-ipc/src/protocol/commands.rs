use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    Direction, DisplaySelector, ResizeOrientation, RestoreScope, RestoreSource, WindowId,
    WorkspaceSelector,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, strum::IntoStaticStr)]
#[cfg_attr(test, derive(strum::VariantNames))]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum LayoutCommand {
    NextWindow,
    PrevWindow,
    MoveFocus(#[serde(rename = "direction")] Direction),
    MoveNode(Direction),
    ToggleStack,
    /// Fold the selected window into the column on this side, or back out of the column it is in.
    ///
    /// Symmetric, unlike `ToggleStack`, whose fold-out half explodes the whole column so a second
    /// press never gets you back. One binding per side gives the same control both ways.
    ToggleFold(Direction),
    ToggleFocusFloating,
    ToggleWindowFloating,
    /// niri's `maximize-column`: the column fills the tiling area and stays in the strip.
    ToggleFullscreenWithinGaps,
    /// Change the selection's size by `amount` of the tiling area: its column's width, its height
    /// in a stacked column, or whichever of the two `Smart` picks.
    ResizeWindowBy {
        amount: f64,
        #[serde(default)]
        orientation: ResizeOrientation,
    },
    SnapStrip,
    /// Cycle the selected column through the configured preset widths.
    ///
    /// niri's `switch-preset-column-width`. `ResizeWindowBy` leaves a column at whatever width
    /// it reaches; this snaps to a known set instead.
    ///
    /// The whole viewport is one of those widths, a width rather than full-width mode:
    /// the cycle reaches it with the column's stack kept, and a step off a full-width
    /// window unmaximises it.
    CyclePresetColumnWidth,
    CenterSelection,
    NextWorkspace(Option<bool>),
    PrevWorkspace(Option<bool>),
    SwitchToWorkspace(usize),
    MoveWindowToWorkspace {
        workspace: WorkspaceSelector,
        follow: bool,
        window_id: Option<u32>,
    },
    SwitchToLastWorkspace,
    SwapWindows(WindowId, WindowId),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, strum::IntoStaticStr)]
#[cfg_attr(test, derive(strum::VariantNames))]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ReactorCommand {
    SaveLayout {
        path: PathBuf,
    },
    SaveAndExit,
    RestoreLayout {
        path: PathBuf,
        scope: RestoreScope,
        #[serde(default)]
        source: RestoreSource,
    },
    SwitchSpace(Direction),
    ToggleSpaceActivated,
    /// Spread each display's windows back across workspaces by their recorded affinity.
    ///
    /// Recovery for a state where windows have piled into one workspace. Before this the only
    /// remedy was deleting the layout file.
    RedistributeWindows,
    FocusWindow {
        window_id: WindowId,
        window_server_id: Option<u32>,
    },
    MoveMouseToDisplay(DisplaySelector),
    FocusDisplay(DisplaySelector),
    CloseWindow {
        window_server_id: Option<u32>,
    },
    MoveWindowToDisplay {
        selector: DisplaySelector,
        window_id: Option<u32>,
    },
    /// One step of the window switcher over `scope`, focusing the window it lands on wherever it is.
    ///
    /// The scope decides what a step moves between as well as which windows — see `SwitchScope`.
    SwitchWindow {
        #[serde(default)]
        backward: bool,
        #[serde(default)]
        scope: SwitchScope,
    },
}

/// Which windows a switch offers, and what a step moves between. Every scope is most recently focused
/// first.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchScope {
    /// Every window, on every workspace and display, one step per application: each application's
    /// windows are a stack, and a step lands on its most recently focused one.
    #[default]
    Everything,
    /// The focused workspace's windows on EVERY display, one step per window. A workspace is one
    /// context spread across displays, so a switch scoped to it is not scoped to a display.
    Workspace,
    /// The focused application's windows, wherever they are, one step per window. cmd-`.
    ///
    /// macOS's own cmd-` only offers windows on the current Space, so with three Ghostty windows over
    /// two rini workspaces it never reached the third. This offers every one of them.
    App,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigCommand {
    /// A KDL fragment laid over the running config until the next reload. It is not appended: what
    /// it writes replaces what is running, setting by setting, `workspace` and `window-rule` lists
    /// whole, and binds key by key (`specs/config.md`, "Changing the running config").
    Set(String),
    ReloadConfig,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiniCommand {
    Layout(LayoutCommand),
    Reactor(ReactorCommand),
    Config(ConfigCommand),
}

/// The part of [`RiniCommand`] the window manager itself executes; `Config` goes to the config
/// actor instead. Untagged, so a bare command name parses whichever arm accepts it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum Command {
    Layout(LayoutCommand),
    Reactor(ReactorCommand),
}

#[cfg(test)]
mod tests {
    use serde::de::{self, Deserializer, Visitor};
    use strum::VariantNames;

    use super::*;

    /// The names serde reads `T`'s variants by: what a client sends.
    fn wire_names<'de, T: Deserialize<'de>>() -> &'static [&'static str] {
        struct Names(&'static [&'static str]);
        impl<'de> Deserializer<'de> for &mut Names {
            type Error = de::value::Error;

            fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
                Err(de::Error::custom("not an enum"))
            }

            fn deserialize_enum<V: Visitor<'de>>(
                self,
                _: &'static str,
                variants: &'static [&'static str],
                _: V,
            ) -> Result<V::Value, Self::Error> {
                self.0 = variants;
                Err(de::Error::custom("read"))
            }

            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
                option unit unit_struct newtype_struct seq tuple tuple_struct map struct identifier
                ignored_any
            }
        }
        let mut names = Names(&[]);
        let _ = T::deserialize(&mut names);
        names.0
    }

    #[test]
    fn strum_names_are_the_wire_names() {
        assert_eq!(LayoutCommand::VARIANTS, wire_names::<LayoutCommand>());
        assert_eq!(ReactorCommand::VARIANTS, wire_names::<ReactorCommand>());
        let named = |command: &LayoutCommand| -> &'static str { command.into() };
        assert_eq!(named(&LayoutCommand::MoveFocus(Direction::Left)), "move_focus");
        let named = |command: &ReactorCommand| -> &'static str { command.into() };
        let window_id = WindowId { pid: 1, idx: 2 };
        let focus = ReactorCommand::FocusWindow { window_id, window_server_id: None };
        assert_eq!(named(&focus), "focus_window");
    }
}
