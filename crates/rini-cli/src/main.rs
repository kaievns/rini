use std::io::{self, Write};
use std::process::{self};

use clap::{Parser, Subcommand, ValueEnum};
use rini_core::ids::WindowId as InternalWindowId;
use rini_core::ids::WindowServerId;
use rini_ipc::RiniMachClient;
use rini_ipc::protocol::WorkspaceSelector;
use rini_ipc::protocol::{self as reactor, DisplaySelector};
use rini_ipc::protocol::{RiniCommand, RiniRequest, RiniResponse, SwitchScope};
use serde::Serialize;

#[derive(Parser)]
#[command(name = "rini-cli")]
#[command(about = "Command-line interface for rini window manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Query information from rini
    Query {
        #[command(subcommand)]
        query: QueryCommands,
    },
    /// Execute commands in rini
    Execute {
        #[command(subcommand)]
        command: ExecuteCommands,
    },
}

#[derive(Subcommand)]
enum QueryCommands {
    /// List virtual workspaces (optionally for a specific MacOS space)
    Workspaces {
        #[arg(long)]
        space_id: Option<u64>,
    },
    /// List windows (optionally filtered by space)
    Windows {
        #[arg(long)]
        space_id: Option<u64>,
    },
    /// List connected displays
    Displays,
    /// Get information about a specific window
    Window { window_id: String },
    /// List running applications
    Applications,
    /// Get layout state and normalized container tree for a space
    Layout {
        /// macOS space ID; defaults to the active display space
        #[arg(long)]
        space_id: Option<u64>,
        /// Virtual workspace number, counting from 0; defaults to the active workspace
        #[arg(long)]
        workspace_id: Option<usize>,
    },
    /// Get performance metrics
    Metrics,
    /// Dump strip and display state for every space.
    ///
    /// Use this instead of `windows` when investigating multi-monitor behaviour:
    /// `windows` defaults to a single space's active workspace, so windows on the
    /// other display look absent when they are simply not in the queried space.
    Diagnostics,
}

#[derive(Subcommand)]
enum ExecuteCommands {
    /// Window management commands
    Window {
        #[command(subcommand)]
        window_cmd: WindowCommands,
    },
    /// Virtual workspace commands
    Workspace {
        #[command(subcommand)]
        workspace_cmd: WorkspaceCommands,
    },
    /// Layout commands
    Layout {
        #[command(subcommand)]
        layout_cmd: LayoutCommands,
    },
    /// Configuration management commands
    Config {
        #[command(subcommand)]
        config_cmd: ConfigCommands,
    },
    /// Display/mouse commands
    Display {
        #[command(subcommand)]
        display_cmd: DisplayCommands,
    },
    /// macOS space commands (Mission Control spaces, not virtual workspaces)
    Space {
        #[command(subcommand)]
        space_cmd: SpaceCommands,
    },
    /// Save the layout file and exit Rini
    SaveAndExit,
    /// Move every window back to the display it belongs to, keeping its workspace.
    ///
    /// Recovery for a layout where windows have piled onto one display. Does nothing when
    /// every window is already where its recorded affinity says it belongs.
    Redistribute,
    /// Step through every application with a window, on every workspace and display, by its most
    /// recently focused window, focusing that window.
    ///
    /// This is the out-of-band path that works with no event tap at all, which is the only thing
    /// that helps if the keyboard tap has been stood down.
    SwitchWindow {
        /// Step backward through the order.
        #[arg(long)]
        backward: bool,
    },
    /// Step through the focused workspace's windows on every display, most recently focused first.
    SwitchWorkspaceWindow {
        /// Step backward through the order.
        #[arg(long)]
        backward: bool,
    },
    /// Step through the focused app's windows, across workspaces and displays, most recently
    /// focused first.
    ///
    /// macOS's own cmd-` only reaches windows on the visible workspace. This offers all of them and
    /// switches the owning display's workspace to follow. A one-shot step toggles the two most
    /// recent; `window focus --window-id` reaches any other.
    SwitchAppWindow {
        /// Step backward through the order.
        #[arg(long)]
        backward: bool,
    },
}

#[derive(Subcommand)]
enum WindowCommands {
    /// Focus the next window
    Next,
    /// Focus the previous window
    Prev,
    /// Focus a window by direction or by a specific window ID
    Focus {
        /// Direction to focus (left, right, up, down)
        direction: Option<String>,
        /// Rini window ID as JSON (`{"pid":123,"idx":456}`) or debug text
        #[arg(long, conflicts_with = "direction")]
        window_id: Option<String>,
        /// Optional macOS window server ID for the target window
        #[arg(long, requires = "window_id")]
        window_server_id: Option<String>,
    },
    /// Toggle window floating state
    ToggleFloat,
    /// Toggle the selected column filling the tiling area, still scrolling with the strip
    ToggleFullscreenWithinGaps,
    /// Fold the selected window into the column on this side, or back out to where it came from
    ToggleFold { direction: String },
    /// Resize the selected window by a fractional amount.
    /// - Pass a signed floating value: positive to grow, negative to shrink.
    /// - The value is a fraction of the tiling area (e.g. `0.05` = 5%).
    /// Examples:
    ///   rini-cli execute window resize-by 0.05                          # widen by 5%
    ///   rini-cli execute window resize-by -0.10 --orientation vertical  # 10% shorter
    ResizeBy {
        #[arg(allow_hyphen_values = true)]
        amount: f64,
        /// Axis to resize; smart chooses the nearest applicable split.
        #[arg(long, value_enum, default_value_t = CliResizeOrientation::Horizontal)]
        orientation: CliResizeOrientation,
    },
    /// Close a window as if Command-W was pressed
    Close {
        /// Optional window server ID; defaults to the focused window
        #[arg(long, visible_alias = "window-server-id")]
        window_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliResizeOrientation {
    Horizontal,
    Vertical,
    Smart,
}

impl From<CliResizeOrientation> for rini_ipc::protocol::ResizeOrientation {
    fn from(value: CliResizeOrientation) -> Self {
        match value {
            CliResizeOrientation::Horizontal => Self::Horizontal,
            CliResizeOrientation::Vertical => Self::Vertical,
            CliResizeOrientation::Smart => Self::Smart,
        }
    }
}

#[derive(Subcommand)]
enum SpaceCommands {
    /// Toggle whether rini manages the current macOS space
    ToggleActivated,
}

#[derive(Subcommand)]
enum WorkspaceCommands {
    /// Switch to next workspace
    Next { skip_empty: Option<bool> },
    /// Switch to previous workspace
    Prev { skip_empty: Option<bool> },
    /// Switch to specific workspace
    Switch {
        /// The workspace's number, counting from 0: 0 is the config's `focus-workspace 1`
        workspace_id: usize,
    },
    /// Move current window to workspace
    MoveWindow {
        /// The destination workspace's number, counting from 0: 0 is the config's first
        workspace_id: usize,
        /// Switch to the destination workspace after moving the window.
        #[arg(long)]
        follow: bool,
        window_id: Option<u32>,
    },
    /// Switch to the last workspace
    Last,
}

#[derive(Subcommand)]
enum LayoutCommands {
    /// Move the selected node in a direction
    MoveNode { direction: String },
    /// Toggle stacked state for the selected container
    ToggleStack,
    /// Toggle floating on the focused selection (tree focus)
    ToggleFocusFloat,
    /// Swap two windows by window id (`WindowId { pid: ..., idx: ... }`)
    SwapWindows { a: String, b: String },
    /// Snap the strip to the nearest column boundary (scrolling layout only)
    SnapStrip,
    /// Toggle centering of the selected column in scrolling layout.
    /// If invoked again on the same selection, centering is removed.
    CenterSelection,
    /// Cycle the selected column through the configured preset widths, the
    /// whole viewport included (a width, not full-width mode; scrolling layout
    /// only)
    CyclePresetColumnWidth,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Apply a KDL fragment over the running config, e.g. `set 'layout { gaps 8; }'`. A section's
    /// settings replace the running ones, binds replace the binds for their keys, and `workspace`
    /// or `window-rule` nodes replace the running list. Lasts until the next reload.
    Set {
        /// The fragment, written as in config.kdl.
        kdl: String,
    },

    /// Print the running config as JSON
    Get,

    /// Read the config file again now
    Reload,
}

#[derive(Subcommand)]
enum DisplayCommands {
    /// Focus a display by direction, index, or UUID.
    Focus {
        /// Direction relative to the current display (left, right, up, down).
        #[arg(long)]
        direction: Option<String>,
        /// Display index (0-based).
        #[arg(long)]
        index: Option<usize>,
        /// Display UUID.
        #[arg(long)]
        uuid: Option<String>,
    },
    /// Move mouse cursor to a display by index (0-based)
    MoveMouseToIndex {
        /// Display index (0-based)
        index: usize,
    },
    /// Move mouse cursor to a display by UUID
    MoveMouseToUuid {
        /// Display UUID
        uuid: String,
    },
    /// Move a window to a display by direction, index, or UUID.
    MoveWindow {
        /// Direction relative to the window's current display (left, right, up, down).
        #[arg(long)]
        direction: Option<String>,
        /// Display index (0-based).
        #[arg(long)]
        index: Option<usize>,
        /// Display UUID.
        #[arg(long)]
        uuid: Option<String>,
        /// Optional window id (window idx); defaults to the focused window if omitted.
        #[arg(long)]
        window_id: Option<u32>,
    },
}

fn main() {
    sigpipe::reset();
    let cli = Cli::parse();

    let request = match build_request(cli.command) {
        Ok(req) => req,
        Err(e) => {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    };

    let client = match RiniMachClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to rini: {}", e);
            process::exit(1);
        }
    };

    // Send request and handle response.
    match client.send_request(&request) {
        Ok(resp) => match resp {
            RiniResponse::Success { data } => {
                if let Err(e) = write_json(
                    &data,
                    std::env::var("RINI_CLI_PRETTY").map(|v| v != "0").unwrap_or(false),
                ) {
                    eprintln!("Failed to handle response: {}", e);
                    process::exit(1);
                }
            }
            RiniResponse::Error { error } => {
                match serde_json::to_string_pretty(&error) {
                    Ok(pretty) => eprintln!("{}", pretty),
                    Err(_) => eprintln!("Error: {}", error),
                }
                process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("Communication error: {}", e);
            eprintln!("Hint: ensure the rini service is running (try `rini service start`).");
            process::exit(1);
        }
    }
}

fn build_request(command: Commands) -> Result<RiniRequest, String> {
    match command {
        Commands::Query { query } => build_query_request(query),
        Commands::Execute { command } => build_execute_request(command),
    }
}

fn build_query_request(query: QueryCommands) -> Result<RiniRequest, String> {
    match query {
        QueryCommands::Workspaces { space_id } => Ok(RiniRequest::GetWorkspaces { space_id }),
        QueryCommands::Windows { space_id } => Ok(RiniRequest::GetWindows { space_id }),
        QueryCommands::Displays => Ok(RiniRequest::GetDisplays),
        QueryCommands::Window { window_id } => {
            let window_id = protocol_window_id(&parse_window_id(&window_id)?)?;
            Ok(RiniRequest::GetWindowInfo { window_id })
        }
        QueryCommands::Applications => Ok(RiniRequest::GetApplications),
        QueryCommands::Layout { space_id, workspace_id } => {
            Ok(RiniRequest::GetLayoutState { space_id, workspace_id })
        }
        QueryCommands::Metrics => Ok(RiniRequest::GetMetrics),
        QueryCommands::Diagnostics => Ok(RiniRequest::GetDiagnostics),
    }
}

/// One step of the window switcher over `scope`.
fn switch(backward: bool, scope: SwitchScope) -> RiniCommand {
    RiniCommand::Reactor(reactor::ReactorCommand::SwitchWindow { backward, scope })
}

fn build_execute_request(execute: ExecuteCommands) -> Result<RiniRequest, String> {
    let command = match execute {
        ExecuteCommands::Window { window_cmd } => map_window_command(window_cmd)?,
        ExecuteCommands::Workspace { workspace_cmd } => map_workspace_command(workspace_cmd),
        ExecuteCommands::Layout { layout_cmd } => map_layout_command(layout_cmd)?,
        ExecuteCommands::Config { config_cmd } => return Ok(map_config_command(config_cmd)),
        ExecuteCommands::Display { display_cmd } => map_display_command(display_cmd)?,
        ExecuteCommands::Space { space_cmd } => map_space_command(space_cmd),
        ExecuteCommands::SaveAndExit => RiniCommand::Reactor(reactor::ReactorCommand::SaveAndExit),
        ExecuteCommands::Redistribute => {
            RiniCommand::Reactor(reactor::ReactorCommand::RedistributeWindows)
        }
        ExecuteCommands::SwitchWindow { backward } => switch(backward, SwitchScope::Everything),
        ExecuteCommands::SwitchWorkspaceWindow { backward } => {
            switch(backward, SwitchScope::Workspace)
        }
        ExecuteCommands::SwitchAppWindow { backward } => switch(backward, SwitchScope::App),
    };
    Ok(RiniRequest::ExecuteCommand { command })
}

fn map_window_command(cmd: WindowCommands) -> Result<RiniCommand, String> {
    use rini_ipc::protocol::LayoutCommand as LC;
    Ok(match cmd {
        WindowCommands::Next => RiniCommand::Layout(LC::NextWindow),
        WindowCommands::Prev => RiniCommand::Layout(LC::PrevWindow),
        WindowCommands::Focus {
            direction,
            window_id,
            window_server_id,
        } => match (direction, window_id) {
            (Some(direction), None) => {
                RiniCommand::Layout(LC::MoveFocus(parse_direction(&direction)?))
            }
            (None, Some(window_id)) => {
                RiniCommand::Reactor(reactor::ReactorCommand::FocusWindow {
                    window_id: parse_window_id(&window_id)?.into(),
                    window_server_id: window_server_id
                        .as_deref()
                        .map(parse_window_server_id)
                        .map(|result| result.map(|id| id.as_u32()))
                        .transpose()?,
                })
            }
            (None, None) => {
                return Err("window focus requires a direction or --window-id".to_string());
            }
            (Some(_), Some(_)) => {
                return Err(
                    "window focus accepts either a direction or --window-id, not both".to_string()
                );
            }
        },
        WindowCommands::ToggleFloat => RiniCommand::Layout(LC::ToggleWindowFloating),
        WindowCommands::ToggleFullscreenWithinGaps => {
            RiniCommand::Layout(LC::ToggleFullscreenWithinGaps)
        }
        WindowCommands::ToggleFold { direction } => {
            RiniCommand::Layout(LC::ToggleFold(parse_direction(&direction)?))
        }
        WindowCommands::ResizeBy { amount, orientation } => RiniCommand::Layout(
            LC::ResizeWindowBy { amount, orientation: orientation.into() },
        ),
        WindowCommands::Close { window_id } => {
            let window_server_id = window_id.as_deref().map(parse_window_server_id).transpose()?;
            RiniCommand::Reactor(reactor::ReactorCommand::CloseWindow {
                window_server_id: window_server_id.map(|id| id.as_u32()),
            })
        }
    })
}

fn parse_window_server_id(input: &str) -> Result<WindowServerId, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("window_server_id cannot be empty".to_string());
    }

    let value = if trimmed.starts_with("0x") {
        u32::from_str_radix(trimmed.trim_start_matches("0x"), 16)
            .map_err(|_| format!("Invalid hexadecimal window server id: {}", trimmed))?
    } else {
        trimmed.parse().map_err(|_| format!("Invalid window server id: {}", trimmed))?
    };
    Ok(WindowServerId::new(value))
}

fn parse_window_id(input: &str) -> Result<InternalWindowId, String> {
    let input = input.trim();
    if let Ok(window_id) = serde_json::from_str(input) {
        return Ok(window_id);
    }
    if let Some(window_id) = InternalWindowId::from_debug_string(input) {
        return Ok(window_id);
    }

    Err(format!(
        "Invalid window id '{}'; expected `{{\"pid\":123,\"idx\":456}}` or `WindowId {{ pid: 123, idx: 456 }}`",
        input
    ))
}

fn protocol_window_id(
    window_id: &InternalWindowId,
) -> Result<rini_ipc::protocol::WindowId, String> {
    rini_ipc::protocol::WindowId::new(window_id.pid, window_id.idx.get())
        .ok_or_else(|| "window id index must be non-zero".to_string())
}

fn map_workspace_command(cmd: WorkspaceCommands) -> RiniCommand {
    use rini_ipc::protocol::LayoutCommand as LC;
    RiniCommand::Layout(match cmd {
        WorkspaceCommands::Next { skip_empty } => LC::NextWorkspace(skip_empty),
        WorkspaceCommands::Prev { skip_empty } => LC::PrevWorkspace(skip_empty),
        WorkspaceCommands::Switch { workspace_id } => LC::SwitchToWorkspace(workspace_id),
        WorkspaceCommands::MoveWindow {
            workspace_id,
            follow,
            window_id,
        } => LC::MoveWindowToWorkspace {
            workspace: WorkspaceSelector::Index(workspace_id),
            follow,
            window_id,
        },
        WorkspaceCommands::Last => LC::SwitchToLastWorkspace,
    })
}

fn map_layout_command(cmd: LayoutCommands) -> Result<RiniCommand, String> {
    use rini_ipc::protocol::LayoutCommand as LC;
    Ok(RiniCommand::Layout(match cmd {
        LayoutCommands::MoveNode { direction } => LC::MoveNode(parse_direction(&direction)?),
        LayoutCommands::ToggleStack => LC::ToggleStack,
        LayoutCommands::ToggleFocusFloat => LC::ToggleFocusFloating,
        LayoutCommands::SwapWindows { a, b } => {
            LC::SwapWindows(parse_window_id(&a)?.into(), parse_window_id(&b)?.into())
        }
        LayoutCommands::SnapStrip => LC::SnapStrip,
        LayoutCommands::CenterSelection => LC::CenterSelection,
        LayoutCommands::CyclePresetColumnWidth => LC::CyclePresetColumnWidth,
    }))
}

/// `config get` is a query of its own; setting and reloading are config commands.
fn map_config_command(cmd: ConfigCommands) -> RiniRequest {
    use rini_ipc::protocol::ConfigCommand;
    let command = match cmd {
        ConfigCommands::Get => return RiniRequest::GetConfig,
        ConfigCommands::Set { kdl } => ConfigCommand::Set(kdl),
        ConfigCommands::Reload => ConfigCommand::ReloadConfig,
    };
    RiniRequest::ExecuteCommand { command: RiniCommand::Config(command) }
}

fn map_space_command(cmd: SpaceCommands) -> RiniCommand {
    match cmd {
        SpaceCommands::ToggleActivated => {
            RiniCommand::Reactor(reactor::ReactorCommand::ToggleSpaceActivated)
        }
    }
}

fn map_display_command(cmd: DisplayCommands) -> Result<RiniCommand, String> {
    use reactor::ReactorCommand as RC;
    Ok(RiniCommand::Reactor(match cmd {
        DisplayCommands::Focus { direction, index, uuid } => {
            RC::FocusDisplay(build_display_selector(direction, index, uuid)?)
        }
        DisplayCommands::MoveMouseToIndex { index } => {
            RC::MoveMouseToDisplay(DisplaySelector::Index(index))
        }
        DisplayCommands::MoveMouseToUuid { uuid } => {
            RC::MoveMouseToDisplay(DisplaySelector::Uuid(uuid))
        }
        DisplayCommands::MoveWindow {
            direction,
            index,
            uuid,
            window_id,
        } => RC::MoveWindowToDisplay {
            selector: build_display_selector(direction, index, uuid)?,
            window_id,
        },
    }))
}

fn build_display_selector(
    direction: Option<String>,
    index: Option<usize>,
    uuid: Option<String>,
) -> Result<DisplaySelector, String> {
    let provided =
        direction.is_some() as usize + index.is_some() as usize + uuid.is_some() as usize;
    if provided != 1 {
        return Err(
            "display selection requires exactly one of --direction, --index, or --uuid".to_string(),
        );
    }

    if let Some(direction) = direction {
        let parsed_direction = parse_direction(&direction)?;
        Ok(DisplaySelector::Direction(parsed_direction))
    } else if let Some(index) = index {
        Ok(DisplaySelector::Index(index))
    } else if let Some(uuid) = uuid {
        Ok(DisplaySelector::Uuid(uuid))
    } else {
        unreachable!("At least one selector value is guaranteed to be provided")
    }
}

fn parse_direction(value: &str) -> Result<rini_ipc::protocol::Direction, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" => Ok(rini_ipc::protocol::Direction::Left),
        "right" => Ok(rini_ipc::protocol::Direction::Right),
        "up" => Ok(rini_ipc::protocol::Direction::Up),
        "down" => Ok(rini_ipc::protocol::Direction::Down),
        other => Err(format!(
            "Invalid direction '{}'; must be left, right, up, or down",
            other
        )),
    }
}

fn write_json<T: Serialize>(value: &T, pretty: bool) -> Result<(), String> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let mut writer = io::BufWriter::new(&mut handle);

    if pretty {
        serde_json::to_writer_pretty(&mut writer, value).map_err(|e| e.to_string())?;
    } else {
        serde_json::to_writer(&mut writer, value).map_err(|e| e.to_string())?;
    }
    writer.write_all(b"\n").map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_requests_use_typed_protocol_commands() {
        let request = build_execute_request(ExecuteCommands::Window {
            window_cmd: WindowCommands::Next,
        })
        .unwrap();

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "execute_command": { "command": { "layout": "next_window" } }
            })
        );
    }

    #[test]
    fn config_commands_are_not_embedded_as_json_strings() {
        let request = build_execute_request(ExecuteCommands::Config {
            config_cmd: ConfigCommands::Set { kdl: "layout { gaps 8; }".into() },
        })
        .unwrap();

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "execute_command": { "command": { "config": { "set": "layout { gaps 8; }" } } }
            })
        );
    }
    #[test]
    fn the_three_switch_subcommands_are_the_three_scopes() {
        for (subcommand, scope) in [
            ("switch-window", SwitchScope::Everything),
            ("switch-workspace-window", SwitchScope::Workspace),
            ("switch-app-window", SwitchScope::App),
        ] {
            let cli = Cli::try_parse_from(["rini-cli", "execute", subcommand, "--backward"])
                .unwrap_or_else(|e| panic!("{subcommand}: {e}"));
            let Commands::Execute { command } = cli.command else {
                panic!("{subcommand} is not an execute subcommand");
            };
            assert_eq!(
                build_execute_request(command).unwrap(),
                RiniRequest::ExecuteCommand { command: switch(true, scope) },
                "{subcommand}"
            );
        }
    }

    /// The CLI's workspace numbers go to the wire as given, from 0, and a move follows only when
    /// asked: specs/workspaces.md and this crate's README.
    #[test]
    fn a_workspace_number_goes_as_given_and_a_move_follows_only_with_follow() {
        use rini_ipc::protocol::LayoutCommand as LC;
        let wire = |args: &[&str]| {
            let cli = Cli::try_parse_from(["rini-cli", "execute", "workspace"].iter().chain(args))
                .unwrap_or_else(|e| panic!("{args:?}: {e}"));
            let Commands::Execute { command } = cli.command else {
                panic!("{args:?} is not an execute subcommand");
            };
            serde_json::to_value(build_execute_request(command).unwrap()).unwrap()
        };
        let layout = |command: LC| {
            serde_json::to_value(RiniRequest::ExecuteCommand {
                command: RiniCommand::Layout(command),
            })
            .unwrap()
        };
        let moved = |follow| LC::MoveWindowToWorkspace {
            workspace: WorkspaceSelector::Index(2),
            follow,
            window_id: None,
        };
        assert_eq!(wire(&["switch", "0"]), layout(LC::SwitchToWorkspace(0)));
        assert_eq!(wire(&["move-window", "2"]), layout(moved(false)));
        assert_eq!(wire(&["move-window", "2", "--follow"]), layout(moved(true)));
    }

    /// The CLI counts workspaces from 0 and the config from 1, so every help that takes a workspace
    /// number says which (`specs/input.md`).
    #[test]
    fn every_workspace_number_help_says_it_counts_from_0() {
        for args in [
            &["execute", "workspace", "switch", "--help"][..],
            &["execute", "workspace", "move-window", "--help"],
            &["query", "layout", "--help"],
        ] {
            let parsed = Cli::try_parse_from(std::iter::once("rini-cli").chain(args.iter().copied()));
            let help = match parsed {
                Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
                    error.to_string()
                }
                other => panic!("{args:?} printed no help: {:?}", other.err()),
            };
            assert!(help.contains("counting from 0"), "{args:?}:\n{help}");
        }
    }

    /// Kai deleted the scoped manual layouts on 2026-10-15 (`specs/persistence.md`).
    #[test]
    fn no_subcommand_saves_or_loads_a_layout_file() {
        for subcommand in ["save-layout", "load-layout"] {
            let error = Cli::try_parse_from(["rini-cli", "execute", subcommand, "--saved"])
                .err()
                .unwrap_or_else(|| panic!("{subcommand} parsed"));
            assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand, "{subcommand}");
        }
    }

    /// Kai deleted the native Space switch on 2026-10-15 (`specs/displays.md`).
    #[test]
    fn no_subcommand_switches_the_native_space() {
        let error = Cli::try_parse_from(["rini-cli", "execute", "space", "switch", "left"])
            .err()
            .expect("space switch parsed");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    /// Every window subcommand has to map to a DISTINCT wire command. The arms are 33 lines of
    /// near-identical shape, which is exactly where a copy-paste sends two keys to one command and
    /// nothing complains.
    #[test]
    fn no_two_window_subcommands_map_to_the_same_wire_command() {
        let commands = vec![
            ("next", WindowCommands::Next),
            ("prev", WindowCommands::Prev),
            (
                "focus",
                WindowCommands::Focus {
                    direction: Some("left".into()),
                    window_id: None,
                    window_server_id: None,
                },
            ),
            ("toggle-float", WindowCommands::ToggleFloat),
            (
                "toggle-fullscreen-within-gaps",
                WindowCommands::ToggleFullscreenWithinGaps,
            ),
            (
                "toggle-fold",
                WindowCommands::ToggleFold { direction: "left".into() },
            ),
            (
                "resize-by",
                WindowCommands::ResizeBy {
                    amount: 0.05,
                    orientation: CliResizeOrientation::Horizontal,
                },
            ),
        ];
        let mut seen: Vec<(&str, serde_json::Value)> = Vec::new();
        for (label, command) in commands {
            let mapped =
                map_window_command(command).unwrap_or_else(|e| panic!("{label} did not map: {e}"));
            let wire = serde_json::to_value(&mapped).unwrap();
            if let Some((other, _)) = seen.iter().find(|(_, seen)| *seen == wire) {
                panic!("{label} and {other} both map to {wire}");
            }
            seen.push((label, wire));
        }
    }

    /// The two directional fold bindings must differ, which is the whole reason the command carries
    /// a direction.
    #[test]
    fn the_two_fold_directions_are_different_wire_commands() {
        let left =
            map_window_command(WindowCommands::ToggleFold { direction: "left".into() }).unwrap();
        let right =
            map_window_command(WindowCommands::ToggleFold { direction: "right".into() }).unwrap();
        assert_ne!(
            serde_json::to_value(&left).unwrap(),
            serde_json::to_value(&right).unwrap()
        );
    }

    #[test]
    fn a_direction_is_read_case_and_space_insensitively() {
        for text in ["left", "LEFT", " Left "] {
            assert_eq!(
                parse_direction(text).unwrap(),
                rini_ipc::protocol::Direction::Left
            );
        }
        assert!(parse_direction("sideways").is_err());
        assert!(parse_direction("").is_err());
    }

    /// Each orientation has to reach the wire distinctly too, or `--orientation vertical` silently
    /// resizes horizontally.
    #[test]
    fn the_resize_orientations_are_different_wire_commands() {
        let mut seen = Vec::new();
        for orientation in [
            CliResizeOrientation::Horizontal,
            CliResizeOrientation::Vertical,
            CliResizeOrientation::Smart,
        ] {
            let mapped =
                map_window_command(WindowCommands::ResizeBy { amount: 0.05, orientation }).unwrap();
            let wire = serde_json::to_value(&mapped).unwrap();
            assert!(
                !seen.contains(&wire),
                "an orientation duplicates another: {wire}"
            );
            seen.push(wire);
        }
    }

    /// Shrinking is a negative step: `resize-by -0.05` has to reach the wire as one, not be read as
    /// a flag.
    #[test]
    fn a_negative_resize_by_reaches_the_wire_as_a_shrink() {
        let cli = Cli::try_parse_from([
            "rini-cli",
            "execute",
            "window",
            "resize-by",
            "-0.05",
            "--orientation",
            "vertical",
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        let Commands::Execute { command } = cli.command else {
            panic!("resize-by is an execute subcommand");
        };
        assert_eq!(
            build_execute_request(command).unwrap(),
            RiniRequest::ExecuteCommand {
                command: RiniCommand::Layout(rini_ipc::protocol::LayoutCommand::ResizeWindowBy {
                    amount: -0.05,
                    orientation: rini_ipc::protocol::ResizeOrientation::Vertical,
                }),
            }
        );
    }

    /// `config get` asks for the running config; it is not a command the config actor runs.
    #[test]
    fn config_get_is_the_config_query() {
        let request =
            build_execute_request(ExecuteCommands::Config { config_cmd: ConfigCommands::Get })
                .unwrap();
        assert_eq!(request, RiniRequest::GetConfig);
    }

    #[test]
    fn a_window_subcommand_with_a_bad_direction_fails_rather_than_defaulting() {
        assert!(
            map_window_command(WindowCommands::ToggleFold { direction: "sideways".into() })
                .is_err(),
            "a typo must not silently become one of the two real directions"
        );
    }

    #[test]
    fn a_window_server_id_reads_as_decimal_or_hex() {
        assert_eq!(parse_window_server_id("42").unwrap().as_u32(), 42);
        assert_eq!(parse_window_server_id(" 0x2a ").unwrap().as_u32(), 42);
        assert!(parse_window_server_id("").is_err());
        assert!(parse_window_server_id("0xzz").is_err());
        assert!(parse_window_server_id("-1").is_err());
    }

    #[test]
    fn a_window_id_reads_as_json_or_as_its_debug_form() {
        let from_json = parse_window_id(r#"{"pid":123,"idx":456}"#).unwrap();
        let from_debug = parse_window_id("WindowId { pid: 123, idx: 456 }").unwrap();
        assert_eq!(from_json, from_debug);
        assert_eq!(from_json.pid, 123);
        assert_eq!(from_json.idx.get(), 456);
        assert!(parse_window_id("123").is_err());
    }

    /// A zero index is not a window. `WindowId::idx` is a `NonZeroU32`, and the wire type has to
    /// refuse rather than wrap.
    #[test]
    fn a_window_id_index_of_zero_is_refused() {
        assert!(parse_window_id(r#"{"pid":123,"idx":0}"#).is_err());
    }
}
