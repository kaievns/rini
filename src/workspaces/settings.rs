//! What the workspace list can be told about itself: which workspaces there are, which one shows at
//! launch, and the window rules that route windows into them. Plain data with defaults.

use crate::windows::domain::rules::WindowRule;

pub const MAX_WORKSPACES: usize = 128;

#[derive(Debug, PartialEq, Clone)]
pub struct VirtualWorkspaceSettings {
    /// The workspaces, top to bottom. There are as many as there are names.
    pub workspace_names: Vec<String>,
    /// Shown at launch, by index into `workspace_names`.
    pub default_workspace: usize,
    pub reapply_app_rules_on_title_change: bool,
    pub app_rules: Vec<WindowRule>,
}

impl Default for VirtualWorkspaceSettings {
    fn default() -> Self {
        Self {
            workspace_names: ["Main", "Development", "Communication", "Utilities"]
                .map(String::from)
                .to_vec(),
            default_workspace: 0,
            reapply_app_rules_on_title_change: false,
            app_rules: Vec::new(),
        }
    }
}

impl VirtualWorkspaceSettings {
    pub fn workspace_count(&self) -> usize { self.workspace_names.len().clamp(1, MAX_WORKSPACES) }
}

/// `count` workspace names, each its number, for tests that need a count rather than names.
#[cfg(test)]
pub fn numbered_names(count: usize) -> Vec<String> { (1..=count).map(|n| n.to_string()).collect() }
