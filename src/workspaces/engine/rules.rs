//! A window's app rules: what they decide, the workspace and layer they give it, and the size a
//! tiled rule asks for once the window has one.

use objc2_core_foundation::CGRect;
use rini_core::ids::{DisplayUuid, SpaceId, WindowId};
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, warn};

use crate::layout::domain::scrolling::ResizeReport;
use crate::windows::domain::info::AppInfo;
use crate::windows::domain::rules::{AppRuleDecision, RuleState, WindowRuleContext};
use crate::workspaces::domain::app_rules::{
    AfterRules, AppRuleOutcome, AppRuleResize, BeforeRules,
};
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::virtual_workspace::{VirtualWorkspaceId, WorkspaceError};
use crate::workspaces::engine::LayoutEngine;
use crate::workspaces::engine::discovery::Admitted;
use crate::workspaces::{AppRuleEffects, AppRuleResult, RuleFloating, WindowStore};

impl LayoutEngine {
    /// What the app rules say about a window, for a caller that only needs the decision.
    ///
    /// The rule engine lives here because the workspace assignment reads it. A display pin is read by
    /// the reactor instead, which is the only place that knows both the application's name and which
    /// screens are attached.
    pub fn evaluate_app_rules(
        &self,
        app_bundle_id: Option<&str>,
        app_name: Option<&str>,
        window_title: Option<&str>,
        ax_role: Option<&str>,
        ax_subrole: Option<&str>,
        is_modal: bool,
    ) -> AppRuleDecision {
        self.app_rules.evaluate(WindowRuleContext {
            app_bundle_id,
            app_name,
            window_title,
            ax_role,
            ax_subrole,
            is_modal,
            state: RuleState::default(),
        })
    }

    /// `assign_window_with_app_info` with the window's own title, role, subrole and modality read
    /// from the store. `Err(AssignmentFailed)` when the store lacks the window.
    pub fn assign_window_by_rules(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        window_id: WindowId,
        space: SpaceId,
        app_info: Option<&AppInfo>,
    ) -> Result<AppRuleResult, WorkspaceError> {
        let Some(window) = window_store.window(window_id) else {
            return Err(WorkspaceError::AssignmentFailed);
        };
        let title = window.info.title.clone();
        let ax_role = window.info.ax_role.clone();
        let ax_subrole = window.info.ax_subrole.clone();
        let is_modal = window.info.is_modal;
        self.assign_window_with_app_info(
            window_store,
            memory,
            window_id,
            space,
            WindowRuleContext {
                app_bundle_id: app_info.and_then(|a| a.bundle_id.as_deref()),
                app_name: app_info.and_then(|a| a.localized_name.as_deref()),
                window_title: Some(title.as_str()),
                ax_role: ax_role.as_deref(),
                ax_subrole: ax_subrole.as_deref(),
                is_modal,
                state: RuleState::default(),
            },
        )
    }

    /// What the layout holds for `window_id` on `space`, taken before its rules are re-evaluated.
    pub fn before_rules(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        window_id: WindowId,
    ) -> BeforeRules {
        BeforeRules {
            assigned: self
                .virtual_workspace_manager
                .workspace_for_window(window_store, space, window_id)
                .is_some(),
            floating: self.floating.is_floating(window_id),
            ignored: window_store.window(window_id).is_some_and(|w| w.ignore_app_rule),
        }
    }

    /// Records a rule verdict on the window (`ignore_app_rule`) and says what the layout must do
    /// next. A failed evaluation is logged and treated as "managed, rules unknown".
    pub fn settle_app_rule(
        &self,
        window_store: &mut WindowStore,
        window_id: WindowId,
        space: SpaceId,
        before: BeforeRules,
        result: &Result<AppRuleResult, WorkspaceError>,
    ) -> AfterRules {
        if let Err(e) = result {
            warn!("Failed to assign window {:?} to workspace: {:?}", window_id, e);
        }
        if let Some(window) = window_store.window_mut(window_id) {
            window.ignore_app_rule = matches!(result, Ok(AppRuleResult::Unmanaged));
        }
        let in_layout_now = self
            .virtual_workspace_manager
            .workspace_for_window(window_store, space, window_id)
            .is_some()
            || self.floating.is_floating(window_id);
        AfterRules::for_result(before, result.as_ref().map_err(|_| ()), in_layout_now)
    }

    /// Places `window_id` by the rules `window` matches as it opens, else where its application's
    /// windows were when it last ran.
    pub fn assign_window_with_app_info(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        window_id: WindowId,
        space: SpaceId,
        window: WindowRuleContext<'_>,
    ) -> Result<AppRuleResult, WorkspaceError> {
        let WindowRuleContext {
            app_bundle_id, window_title, ..
        } = window;
        let decision = self.app_rules.evaluate(window);

        // Where this application's windows were, but only when the config had nothing to say and the
        // window has no assignment already. This is the first sighting of a launching application's
        // window, well before `WindowAdded`, so it is the only place the answer can still be changed.
        if decision == AppRuleDecision::NoMatch
            && self
                .virtual_workspace_manager
                .workspace_for_window(window_store, space, window_id)
                .is_none()
            && let Some(app_id) = app_bundle_id
            && let Some(slot) = self.launch_slot_for_new_window(
                window_store,
                memory,
                window_id,
                app_id,
                window_title,
            )
            && let Some(workspace_id) = self
                .virtual_workspace_manager
                .list_workspaces(space)
                .get(slot.workspace_index)
                .map(|(id, _)| *id)
            && self.virtual_workspace_manager.assign_window_to_workspace(
                window_store,
                space,
                window_id,
                workspace_id,
            )
        {
            memory.affinity.set_window_home_if_absent(window_id, &slot.display_uuid);
            if let Some(width) = slot.width {
                memory.affinity.set_window_width(&slot.display_uuid, window_id, width);
            }
            debug!(
                idx = window_id.idx.get(),
                workspace = slot.workspace_index,
                width = ?slot.width,
                "placing a relaunched window where it was"
            );
            return Ok(AppRuleResult::Managed(AppRuleEffects {
                workspace_id,
                floating: RuleFloating::as_is(self.floating.is_floating(window_id)),
                position: None,
                size: None,
                focus: false,
            }));
        }

        self.virtual_workspace_manager.apply_app_rule_decision(
            window_store,
            window_id,
            space,
            decision,
        )
    }

    pub(super) fn apply_app_rule_outcome(
        &mut self,
        window_store: &mut WindowStore,
        space: SpaceId,
        admitted: Admitted,
        app_rule_outcome: &mut AppRuleOutcome,
        tiled_by_workspace: &mut HashMap<VirtualWorkspaceId, Vec<WindowId>>,
    ) -> Option<(WindowId, VirtualWorkspaceId)> {
        let Admitted { window, was_floating, result } = admitted;
        let AppRuleResult::Managed(effects) = result else {
            return None;
        };
        let should_float = effects.should_float(was_floating);
        if should_float {
            self.floating.add_floating(window);
            self.floating.add_active(space, window.pid, window);
        } else if was_floating {
            self.floating.remove_floating(window);
        }

        if let Some(placement) = effects.floating_placement(window, space) {
            app_rule_outcome.push_placement(placement);
        } else if let Some(resize) = effects.tiled_resize(window, space, was_floating) {
            app_rule_outcome.push_resize(resize);
        }

        if !should_float {
            tiled_by_workspace.entry(effects.workspace_id).or_default().push(window);
        }

        self.virtual_workspace_manager_mut().set_last_rule_decision(
            window_store,
            window,
            effects.floating.floats(),
        );

        effects.focus.then_some((window, effects.workspace_id))
    }

    pub fn apply_app_rule_resize(
        &mut self,
        resize: AppRuleResize,
        new_frame: CGRect,
        screen_frame: CGRect,
        display_uuid: Option<&DisplayUuid>,
    ) {
        let Some(layout) = self.workspace_layouts.active(resize.space, resize.workspace_id) else {
            return;
        };
        let gaps = self.layout_settings.gaps.effective_for_display(display_uuid);
        let previous_selection = self.workspace_tree(resize.workspace_id).selected_window(layout);
        let border = self.borders.of(resize.window);
        let tree = self.workspace_tree_mut(resize.workspace_id);
        let _ = tree.select_window(layout, resize.window);
        tree.on_window_resized(
            layout,
            ResizeReport {
                window: resize.window,
                drag: None,
                frame: new_frame,
                border,
            },
            screen_frame,
            &gaps,
        );
        if let Some(previous) = previous_selection.filter(|window| *window != resize.window) {
            let _ = tree.select_window(layout, previous);
        }
        self.workspace_layouts
            .mark_last_saved(resize.space, resize.workspace_id, layout);
    }
}
