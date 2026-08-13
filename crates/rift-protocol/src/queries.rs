use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{Direction, LayoutKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct WindowId {
    pub pid: i32,
    pub idx: u32,
}

impl<'de> Deserialize<'de> for WindowId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct WindowIdVisitor;

        impl<'de> Visitor<'de> for WindowIdVisitor {
            type Value = WindowId;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "a window id object, tuple, or debug string like `WindowId { pid: 123, idx: 456 }`",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = value
                    .strip_prefix("WindowId { pid: ")
                    .and_then(|value| value.strip_suffix(" }"))
                    .ok_or_else(|| E::custom("invalid WindowId debug string"))?;
                let (pid, idx) = value
                    .split_once(", idx: ")
                    .ok_or_else(|| E::custom("invalid WindowId debug string"))?;
                let pid = pid.parse().map_err(|_| E::custom("invalid WindowId pid"))?;
                let idx = idx.parse().map_err(|_| E::custom("invalid WindowId idx"))?;
                WindowId::new(pid, idx).ok_or_else(|| E::custom("window id index must be non-zero"))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let pid =
                    sequence.next_element()?.ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let idx =
                    sequence.next_element()?.ok_or_else(|| de::Error::invalid_length(1, &self))?;
                WindowId::new(pid, idx)
                    .ok_or_else(|| de::Error::custom("window id index must be non-zero"))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut pid = None;
                let mut idx = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "pid" => pid = Some(map.next_value()?),
                        "idx" => idx = Some(map.next_value()?),
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                let pid = pid.ok_or_else(|| de::Error::missing_field("pid"))?;
                let idx = idx.ok_or_else(|| de::Error::missing_field("idx"))?;
                WindowId::new(pid, idx)
                    .ok_or_else(|| de::Error::custom("window id index must be non-zero"))
            }
        }

        deserializer.deserialize_any(WindowIdVisitor)
    }
}

impl WindowId {
    pub const fn new(pid: i32, idx: u32) -> Option<Self> {
        if idx == 0 {
            None
        } else {
            Some(Self { pid, idx })
        }
    }

    pub fn to_debug_string(self) -> String {
        format!("WindowId {{ pid: {}, idx: {} }}", self.pid, self.idx)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowData {
    pub id: WindowId,
    pub title: String,
    pub frame: Rect,
    pub is_floating: bool,
    pub is_focused: bool,
    pub bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub window_server_id: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceData {
    pub id: String,
    pub index: usize,
    pub name: String,
    pub layout_mode: String,
    pub is_active: bool,
    pub window_count: usize,
    pub windows: Vec<WindowData>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceLayoutData {
    pub id: String,
    pub index: usize,
    pub name: String,
    pub layout_mode: String,
    pub is_active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApplicationData {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub name: String,
    pub is_frontmost: bool,
    pub window_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutStateData {
    pub space_id: u64,
    pub workspace_id: usize,
    pub is_active_workspace: bool,
    pub mode: String,
    pub floating_windows: Vec<WindowId>,
    pub tiled_windows: Vec<WindowId>,
    pub focused_window: Option<WindowId>,
    /// The layout engine's selected window in the queried workspace.
    pub selected_window: Option<WindowId>,
    /// Normalized topology for the queried workspace's tiled layout.
    ///
    /// Internal node IDs are intentionally omitted because they are not stable across layout
    /// mutations. Consumers can identify leaves by `window_id` and other nodes by their path.
    pub container_tree: ContainerTreeNode,
}

/// The type of a node in Rift's normalized layout topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerNodeType {
    Container,
    Window,
    /// An empty slot retained by a layout engine, such as an empty BSP root.
    Placeholder,
}

/// A platform-neutral view of one node in a tiled layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerTreeNode {
    pub node_type: ContainerNodeType,
    /// Split/stack behavior for a container. Window and placeholder nodes use `None`.
    pub layout_kind: Option<LayoutKind>,
    /// This node's relative share within its parent, when the layout engine has one.
    pub weight: Option<f64>,
    pub window_id: Option<WindowId>,
    /// Layout-engine selection, which is distinct from OS window focus.
    pub is_selected: bool,
    pub is_fullscreen: bool,
    pub is_fullscreen_within_gaps: bool,
    /// Semantic role when the mode defines one, such as `master`, `stack`, or `column`.
    pub role: Option<String>,
    /// Pending BSP split direction, if this leaf is preselected for insertion.
    pub pending_split: Option<Direction>,
    pub children: Vec<ContainerTreeNode>,
}

/// One window as the diagnostics dump sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticWindow {
    pub window_id: WindowId,
    pub app: String,
    pub title: String,
    /// Column index in the strip, or `None` for a floating window.
    pub column: Option<usize>,
    /// Row within the column, for stacked columns.
    pub row: Option<usize>,
    pub frame: Rect,
    /// Points of this window actually inside the display. 0-3 means parked
    /// off-strip: macOS refuses a fully off-screen window, so a scrolled-away
    /// column keeps a 1pt sliver on the edge.
    pub visible_width: f64,
    pub is_parked: bool,
    pub is_floating: bool,
    pub is_focused: bool,
    /// Display UUID this window is remembered as belonging to.
    pub home_display: Option<String>,
}

/// Everything needed to reason about one space's strip, in one place.
///
/// Added because diagnosing multi-monitor problems from `query windows` alone led to
/// three wrong conclusions in a row: that query defaults to a single space's active
/// workspace, so windows on the other display looked missing, then dead, then
/// unmanaged. All three were artefacts of the tool rather than real defects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticSpace {
    pub space_id: u64,
    pub display_uuid: Option<String>,
    pub display_name: Option<String>,
    pub display_frame: Rect,
    pub is_active: bool,
    pub mode: String,
    pub workspace_id: Option<String>,
    pub workspace_name: Option<String>,
    pub workspace_index: Option<usize>,
    /// Left edge of every column, in strip coordinates, so a collapse of several
    /// columns onto one parking position is visible directly.
    pub column_origins: Vec<f64>,
    pub column_widths: Vec<f64>,
    pub windows: Vec<DiagnosticWindow>,
    /// Windows this space owns that are NOT in its layout tree. Should always be
    /// empty; anything here is reachable by cmd-tab but invisible to the strip.
    pub orphaned_windows: Vec<WindowId>,
}

/// How many windows belong to one display, independent of what it is showing.
///
/// `DiagnosticSpace` only covers the workspace currently visible on each display, which is a
/// genuine trap: counting windows across the `spaces` list answers "what is on screen now",
/// not "where do these windows live". Tallying that way made a workspace switch look like it
/// had lost 14 windows when they were simply on a workspace that was no longer showing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticDisplayCensus {
    pub display_uuid: String,
    pub display_name: Option<String>,
    /// Windows whose durable home is this display.
    pub homed: usize,
    /// Windows currently ASSIGNED to this display's space, across every workspace. A
    /// persistent gap between this and `homed` is a migration.
    pub assigned: usize,
    /// Per workspace name, how many of this display's windows sit there. Makes a pile-up in
    /// one workspace visible without switching to it.
    pub by_workspace: Vec<(String, usize)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsData {
    pub spaces: Vec<DiagnosticSpace>,
    /// Whole-topology counts, so "did anything migrate" can be answered without switching
    /// workspaces to look.
    #[serde(default)]
    pub census: Vec<DiagnosticDisplayCensus>,
    /// Workspaces holding windows but belonging to no attached display, i.e. left
    /// over from a previous space generation.
    pub orphaned_workspaces: Vec<String>,
    /// Affinity entries whose window no longer exists.
    pub stale_homes: Vec<WindowId>,
    pub windows_managed: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayData {
    pub uuid: String,
    pub name: Option<String>,
    pub screen_id: u32,
    pub frame: Rect,
    pub space: Option<u64>,
    pub is_active_space: bool,
    pub is_active_context: bool,
    pub active_space_ids: Vec<u64>,
    pub inactive_space_ids: Vec<u64>,
}
