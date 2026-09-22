use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    Direction, DisplaySelector, ResizeOrientation, RestoreScope, RestoreSource,
    WindowId, WorkspaceSelector,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutCommand {
    NextWindow,
    PrevWindow,
    MoveFocus(#[serde(rename = "direction")] Direction),
    MoveNode(Direction),
    JoinWindow(Direction),
    ConsumeOrExpelWindow(Direction),
    ToggleStack,
    /// Fold the selected window into the column on this side, or back out of the column it is in.
    ///
    /// Symmetric, unlike `ToggleStack`, whose fold-out half explodes the whole column so a second
    /// press never gets you back. One binding per side gives the same control both ways.
    ToggleFold(Direction),
    UnjoinWindows,
    ToggleFocusFloating,
    ToggleWindowFloating,
    /// niri's `maximize-column`: the column fills the tiling area and stays in the strip.
    ToggleFullscreenWithinGaps,
    ResizeWindowGrow(ResizeOrientation),
    ResizeWindowShrink(ResizeOrientation),
    ResizeWindowBy {
        amount: f64,
    },
    ScrollStrip {
        delta: f64,
    },
    SnapStrip,
    /// Cycle the selected column through the configured preset widths.
    ///
    /// niri's `switch-preset-column-width`. The existing ResizeWindowGrow /
    /// ResizeWindowShrink commands step by a fixed ~5%, which leaves columns at
    /// arbitrary in-between widths; this snaps to a known set instead.
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
    CreateWorkspace,
    SwitchToLastWorkspace,
    SwapWindows(WindowId, WindowId),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactorCommand {
    Debug,
    /// Slide every visible window in from an offset using the capture-based overlay.
    ///
    /// A debugging aid for judging animation quality by eye. It draws pictures of the windows and
    /// never moves a real one, so it is safe to run at any time and leaves no state behind.
    DebugOverlaySlide {
        dx: i32,
        dy: i32,
        duration_ms: u64,
    },
    /// Capture every window the framebuffer route cannot serve, filling the animation snapshot cache.
    ///
    /// Background work only. Nothing is drawn and no window is touched.
    DebugWarmSnapshots,
    Serialize,
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
    /// Cycle focus between the focused app's windows, across workspaces and displays.
    ///
    /// macOS's own cmd-` only offers windows it considers reachable on the current Space, so
    /// with one app's windows spread over several rini workspaces it silently cycles a subset:
    /// three Ghostty windows, only the two sharing a workspace reachable. rini knows where all
    /// of them are, so it can do the full rotation and switch the owning display's workspace to
    /// follow.
    CycleAppWindows {
        /// Reverse order, for a shift-modified binding.
        #[serde(default)]
        backward: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricsCommand {
    ShowTiming,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigCommand {
    SetAnimate(bool),
    SetAnimationDuration(f64),
    SetMouseFollowsFocus(bool),
    SetMouseHidesOnFocus(bool),
    SetFocusFollowsMouse(bool),
    SetOuterGaps {
        top: f64,
        left: f64,
        bottom: f64,
        right: f64,
    },
    SetInnerGaps {
        horizontal: f64,
        vertical: f64,
    },
    SetWorkspaceNames(Vec<String>),
    Set {
        key: String,
        value: Value,
    },
    GetConfig,
    SaveConfig,
    ReloadConfig,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiniCommand {
    Layout(LayoutCommand),
    Metrics(MetricsCommand),
    Reactor(ReactorCommand),
    Config(ConfigCommand),
}

/// The part of [`RiniCommand`] the window manager itself executes; `Config` goes to the config
/// actor instead. Untagged, so a bare command name parses whichever arm accepts it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum Command {
    Layout(LayoutCommand),
    Metrics(MetricsCommand),
    Reactor(ReactorCommand),
}
