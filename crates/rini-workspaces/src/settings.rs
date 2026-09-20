//! What the workspace list can be told about itself: how many, their names, the default, and
//! the app rules that route windows into them. Plain data with defaults and validation.
use serde::{Deserialize, Serialize};

fn yes() -> bool {
    true
}
fn no() -> bool {
    false
}
use rini_ipc::protocol::WorkspaceSelector;
use rini_windows::rules::AppWorkspaceRule;

pub const MAX_WORKSPACES: usize = 128;
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct VirtualWorkspaceSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_workspace_count")]
    pub default_workspace_count: usize,
    #[serde(default = "no")]
    pub workspace_auto_back_and_forth: bool,
    #[serde(default, alias = "prevent_wrapping_around")]
    pub prevent_wrapping: bool,
    #[serde(default = "default_workspace_names")]
    pub workspace_names: Vec<String>,
    #[serde(default)]
    pub default_workspace: usize,
    #[serde(default)]
    pub reapply_app_rules_on_title_change: bool,
    /// Modal windows (`AXModal`) float instead of taking a column. A rule that names `modal`
    /// overrides this for the windows it matches.
    #[serde(default = "yes")]
    pub float_modal_windows: bool,
    #[serde(default)]
    pub app_rules: Vec<AppWorkspaceRule>,
}
impl Default for VirtualWorkspaceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_workspace_count: default_workspace_count(),
            workspace_auto_back_and_forth: false,
            prevent_wrapping: false,
            workspace_names: default_workspace_names(),
            default_workspace: 0,
            reapply_app_rules_on_title_change: false,
            float_modal_windows: true,
            app_rules: Vec::new(),
        }
    }
}
impl VirtualWorkspaceSettings {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.default_workspace_count == 0 {
            issues.push("default_workspace_count must be at least 1".to_string());
        }
        if self.default_workspace_count > MAX_WORKSPACES {
            issues.push(format!(
                "default_workspace_count should not exceed {} for performance reasons",
                MAX_WORKSPACES
            ));
        }

        if self.workspace_names.len() > self.default_workspace_count {
            issues.push("More workspace names provided than default_workspace_count".to_string());
        }

        if self.default_workspace >= self.default_workspace_count {
            issues.push(format!(
                "default_workspace ({}) must be less than default_workspace_count ({})",
                self.default_workspace, self.default_workspace_count
            ));
        }

        // Validate rules and check duplicates in a single pass
        let mut seen_app_ids = rini_shared::collections::HashSet::default();
        let mut seen_app_names = rini_shared::collections::HashSet::default();
        let mut seen_title_regexes = rini_shared::collections::HashSet::default();
        let mut seen_title_substrings = rini_shared::collections::HashSet::default();
        let mut seen_ax_roles = rini_shared::collections::HashSet::default();
        let mut seen_ax_subroles = rini_shared::collections::HashSet::default();

        for (index, rule) in self.app_rules.iter().enumerate() {
            let app_id_empty = rule.app_id.as_ref().map_or(true, |id| id.is_empty());
            if app_id_empty
                && rule.app_name.is_none()
                && rule.title_regex.is_none()
                && rule.title_substring.is_none()
                && rule.ax_role.is_none()
                && rule.ax_subrole.is_none()
                && rule.modal.is_none()
            {
                issues.push(format!(
                    "App rule {} has no app_id, app_name, title_regex, title_substring, ax_role, ax_subrole, or modal specified",
                    index
                ));
            }

            if let Some(ref workspace) = rule.workspace {
                if let WorkspaceSelector::Index(idx) = workspace {
                    if *idx >= self.default_workspace_count {
                        issues.push(format!(
                            "App rule {} references workspace {} but only {} workspaces will be created",
                            index, idx, self.default_workspace_count
                        ));
                    }
                }
            }

            if let Some(position) = rule.position {
                if !position.x.is_finite()
                    || !position.y.is_finite()
                    || !(0.0..=1.0).contains(&position.x)
                    || !(0.0..=1.0).contains(&position.y)
                {
                    issues.push(format!(
                        "App rule {} position x and y must be finite values between 0 and 1",
                        index
                    ));
                }
                if !rule.floating {
                    issues.push(format!(
                        "App rule {} specifies position, but position only applies when floating = true",
                        index
                    ));
                }
            }

            if let Some(size) = rule.size {
                if size.w.is_none() && size.h.is_none() {
                    issues.push(format!(
                        "App rule {} size must specify at least one of w or h",
                        index
                    ));
                }
                if size.w.is_some_and(|value| !value.is_finite() || value <= 0.0)
                    || size.h.is_some_and(|value| !value.is_finite() || value <= 0.0)
                {
                    issues.push(format!(
                        "App rule {} size dimensions must be finite positive values",
                        index
                    ));
                }
            }

            if let Some(ref app_id) = rule.app_id {
                if !app_id.is_empty() && !app_id.contains('.') {
                    issues.push(format!(
                        "App rule {} has suspicious app_id '{}' (should be bundle identifier like 'com.example.app')",
                        index, app_id
                    ));
                }

                let has_specific_match = rule.app_name.is_some()
                    || rule.title_regex.is_some()
                    || rule.title_substring.is_some()
                    || rule.ax_role.is_some()
                    || rule.ax_subrole.is_some();
                if !app_id.is_empty() && !has_specific_match && !seen_app_ids.insert(app_id) {
                    issues.push(format!("Duplicate app_id '{}' in rule {}", app_id, index));
                }
            }

            if let Some(ref app_name) = rule.app_name {
                if !seen_app_names.insert(app_name) {
                    issues.push(format!("Duplicate app_name '{}' in rule {}", app_name, index));
                }
            }

            if let Some(ref title_re) = rule.title_regex {
                if title_re.is_empty() {
                    issues.push(format!("App rule {} has empty title_regex", index));
                } else if !seen_title_regexes.insert(title_re) {
                    issues.push(format!("Duplicate title_regex '{}' in rule {}", title_re, index));
                }
            }

            if let Some(ref title_sub) = rule.title_substring {
                if title_sub.is_empty() {
                    issues.push(format!("App rule {} has empty title_substring", index));
                } else if !seen_title_substrings.insert(title_sub) {
                    issues.push(format!(
                        "Duplicate title_substring '{}' in rule {}",
                        title_sub, index
                    ));
                }
            }

            if let Some(ref ax_role) = rule.ax_role {
                if ax_role.is_empty() {
                    issues.push(format!("App rule {} has empty ax_role", index));
                } else if !seen_ax_roles.insert(ax_role) {
                    issues.push(format!("Duplicate ax_role '{}' in rule {}", ax_role, index));
                }
            }

            if let Some(ref ax_sub) = rule.ax_subrole {
                if ax_sub.is_empty() {
                    issues.push(format!("App rule {} has empty ax_subrole", index));
                } else if !seen_ax_subroles.insert(ax_sub) {
                    issues.push(format!("Duplicate ax_subrole '{}' in rule {}", ax_sub, index));
                }
            }
        }

        issues
    }
}
fn default_workspace_count() -> usize {
    4
}
fn default_workspace_names() -> Vec<String> {
    vec![
        "Main".to_string(),
        "Development".to_string(),
        "Communication".to_string(),
        "Utilities".to_string(),
    ]
}
