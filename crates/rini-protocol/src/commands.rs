use std::path::PathBuf;

use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{
    Direction, DisplaySelector, LayoutMode, ResizeOrientation, RestoreScope, RestoreSource,
    WindowId, WorkspaceSelector,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutCommand {
    NextWindow,
    PrevWindow,
    MoveFocus(#[serde(rename = "direction")] Direction),
    Ascend,
    Descend,
    MoveNode(Direction),
    JoinWindow(Direction),
    ConsumeOrExpelWindow(Direction),
    ToggleStack,
    ToggleOrientation,
    UnjoinWindows,
    ToggleFocusFloating,
    ToggleWindowFloating,
    ToggleFullscreen,
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
    SetWorkspaceLayout {
        workspace: Option<usize>,
        mode: LayoutMode,
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
    ShowMissionControlAll,
    ShowMissionControlCurrent,
    DismissMissionControl,
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
    SetAnimationFps(f64),
    SetAnimationEasing(AnimationEasing),
    SetMouseFollowsFocus(bool),
    SetMouseHidesOnFocus(bool),
    SetFocusFollowsMouse(bool),
    SetStackOffset(f64),
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationEasing {
    #[default]
    EaseInOut,
    Linear,
    EaseInSine,
    EaseOutSine,
    EaseInOutSine,
    EaseInQuad,
    EaseOutQuad,
    EaseInOutQuad,
    EaseInCubic,
    EaseOutCubic,
    EaseInOutCubic,
    EaseInQuart,
    EaseOutQuart,
    EaseInOutQuart,
    EaseInQuint,
    EaseOutQuint,
    EaseInOutQuint,
    EaseInExpo,
    EaseOutExpo,
    EaseInOutExpo,
    EaseInCirc,
    EaseOutCirc,
    EaseInOutCirc,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiniCommand {
    Layout(LayoutCommand),
    Metrics(MetricsCommand),
    Reactor(ReactorCommand),
    Config(ConfigCommand),
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum TypedRiniCommand {
    Layout(LayoutCommand),
    Metrics(MetricsCommand),
    Reactor(ReactorCommand),
    Config(ConfigCommand),
}

#[derive(Deserialize)]
enum LegacyCommand {
    #[serde(alias = "reactor")]
    Reactor(LegacyReactorCommand),
    #[serde(alias = "config")]
    Config(ConfigCommand),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LegacyReactorCommand {
    Layout(LayoutCommand),
    Metrics(MetricsCommand),
    Reactor(ReactorCommand),
}

impl From<TypedRiniCommand> for RiniCommand {
    fn from(command: TypedRiniCommand) -> Self {
        match command {
            TypedRiniCommand::Layout(command) => Self::Layout(command),
            TypedRiniCommand::Metrics(command) => Self::Metrics(command),
            TypedRiniCommand::Reactor(command) => Self::Reactor(command),
            TypedRiniCommand::Config(command) => Self::Config(command),
        }
    }
}

impl<'de> Deserialize<'de> for RiniCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum CommandInput {
            Typed(TypedRiniCommand),
            LegacyJson(String),
        }

        match CommandInput::deserialize(deserializer)? {
            CommandInput::Typed(command) => Ok(command.into()),
            CommandInput::LegacyJson(command) => decode_legacy_command(&command),
        }
    }
}

fn decode_legacy_command<E>(command: &str) -> Result<RiniCommand, E>
where
    E: DeError,
{
    match serde_json::from_str::<LegacyCommand>(command)
        .map_err(|error| E::custom(format!("invalid legacy command JSON: {error}")))?
    {
        LegacyCommand::Config(command) => Ok(RiniCommand::Config(command)),
        LegacyCommand::Reactor(LegacyReactorCommand::Layout(command)) => {
            Ok(RiniCommand::Layout(command))
        }
        LegacyCommand::Reactor(LegacyReactorCommand::Metrics(command)) => {
            Ok(RiniCommand::Metrics(command))
        }
        LegacyCommand::Reactor(LegacyReactorCommand::Reactor(command)) => {
            Ok(RiniCommand::Reactor(command))
        }
    }
}
