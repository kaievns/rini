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
    AfterRules, AppRuleOutcome, AppRuleResize, BeforeRules, SlotOverrides,
};
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::launch_memory::Slot;
use crate::workspaces::domain::virtual_workspace::{VirtualWorkspaceId, WorkspaceError};
use crate::workspaces::engine::LayoutEngine;
use crate::workspaces::engine::discovery::Admitted;
use crate::workspaces::engine::launch::NoLaunchSlot;
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
        let first = self.first_sighting(window_id);

        // Where this application's windows were, for a window with no assignment already, but for
        // what the rules set, and not at all for one they float or leave unmanaged; else, for a
        // window new to the session, the width its app last had on this display shape. This is the
        // first sighting of a launching application's window, well before `WindowAdded`, so it is
        // the only place the answer can still be changed.
        let unassigned = self
            .virtual_workspace_manager
            .workspace_for_window(window_store, space, window_id)
            .is_none();
        if unassigned && let Some(app_id) = app_bundle_id {
            let launch = NewWindow {
                id: window_id,
                space,
                app_id,
                title: window_title,
            };
            match decision {
                AppRuleDecision::NoMatch => {
                    if let Some(effects) =
                        self.place_from_launch_slot(window_store, memory, launch, first)
                    {
                        return Ok(AppRuleResult::Managed(effects));
                    }
                    if first {
                        self.take_app_width(memory, launch);
                    }
                }
                AppRuleDecision::Managed { .. } | AppRuleDecision::Unmanaged => {
                    if let Some(overrides) = SlotOverrides::of(&decision) {
                        let took = self.take_launch_slot_under_rules(
                            window_store,
                            memory,
                            launch,
                            SlotTaking { overrides, first },
                        );
                        if first && !took && !overrides.width {
                            self.take_app_width(memory, launch);
                        }
                    }
                }
            }
        }

        self.virtual_workspace_manager.apply_app_rule_decision(
            window_store,
            window_id,
            space,
            decision,
        )
    }

    /// Puts `window` in the workspace its application's remembered slot names, with the slot's
    /// home and, at its `first` sighting, its width, or logs why it cannot.
    fn place_from_launch_slot(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        window: NewWindow<'_>,
        first: bool,
    ) -> Option<AppRuleEffects> {
        let slot = self.launch_slot_for_new_window(
            window_store,
            memory,
            window.id,
            window.app_id,
            window.title,
        )?;
        let workspace_id = self.assign_slot_workspace(window_store, window, &slot)?;
        take_slot_home_and_width(memory, window.id, &slot, SlotTaking {
            overrides: SlotOverrides::default(),
            first,
        });
        Some(AppRuleEffects {
            workspace_id,
            floating: RuleFloating::as_is(self.floating.is_floating(window.id)),
            position: None,
            size: None,
            focus: false,
        })
    }

    /// Gives `window` what its application's remembered slot holds that the rules applying to it
    /// do not set (`taking.overrides`): the slot's workspace and width, each only where the rules
    /// name none, the width only at a first sighting, and its home, or logs why it takes nothing.
    /// Says whether it took the slot.
    fn take_launch_slot_under_rules(
        &mut self,
        window_store: &mut WindowStore,
        memory: &mut DisplayMemory,
        window: NewWindow<'_>,
        taking: SlotTaking,
    ) -> bool {
        if taking.overrides.all() {
            if self.has_launch_slots(memory, window.app_id) {
                self.log_no_launch_slot(window.id, window.app_id, NoLaunchSlot::RulesSetAll);
            }
            return false;
        }
        let Some(slot) = self.launch_slot_for_new_window(
            window_store,
            memory,
            window.id,
            window.app_id,
            window.title,
        ) else {
            return false;
        };
        if !taking.overrides.workspace
            && self.assign_slot_workspace(window_store, window, &slot).is_none()
        {
            return false;
        }
        take_slot_home_and_width(memory, window.id, &slot, taking);
        true
    }

    /// Assigns `window` to the workspace `slot` names, or logs that it could not.
    fn assign_slot_workspace(
        &mut self,
        window_store: &mut WindowStore,
        window: NewWindow<'_>,
        slot: &Slot,
    ) -> Option<VirtualWorkspaceId> {
        let placed = self
            .virtual_workspace_manager
            .list_workspaces(window.space)
            .get(slot.workspace_index)
            .map(|(id, _)| *id)
            .filter(|workspace_id| {
                self.virtual_workspace_manager.assign_window_to_workspace(
                    window_store,
                    window.space,
                    window.id,
                    *workspace_id,
                )
            });
        if placed.is_none() {
            self.log_no_launch_slot(window.id, window.app_id, NoLaunchSlot::WorkspaceRefused {
                workspace_index: slot.workspace_index,
            });
        }
        placed
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
        } else if let Some(resize) =
            effects.tiled_resize(window, space, was_floating, window_store.last_rule_size(window))
        {
            window_store.set_last_rule_size(window, resize.size);
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

    /// The size a rule asks of a tiled window, taken as its column's width and recorded as the
    /// window's width on this display, so the window reads it back when it is laid out again. A width
    /// the window has of its own there beats the rule's and keeps its column, unless the rule's size
    /// was edited since it last sized the window; the rule's height is taken either way.
    pub fn apply_app_rule_resize(
        &mut self,
        memory: &mut DisplayMemory,
        resize: AppRuleResize,
        new_frame: CGRect,
        screen_frame: CGRect,
        display_uuid: Option<&DisplayUuid>,
    ) {
        let Some(layout) = self.workspace_layouts.active(resize.space, resize.workspace_id) else {
            return;
        };
        let display = memory.affinity.display_for_space(resize.space).cloned();
        let own_width = display
            .as_ref()
            .and_then(|display| memory.affinity.window_width(display, resize.window));
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
        if let Some(display) = display {
            if own_width.is_some() && !resize.edited {
                self.apply_remembered_column_width(
                    memory,
                    resize.space,
                    resize.workspace_id,
                    layout,
                    resize.window,
                );
            } else if let Some(width) =
                self.workspace_tree(resize.workspace_id).carried_width(layout, resize.window)
            {
                memory.affinity.set_window_width(&display, resize.window, width);
            }
        }
        self.workspace_layouts
            .mark_last_saved(resize.space, resize.workspace_id, layout);
    }
}

/// A window seen for the first time, as the rules see it: before it is in the window store.
#[derive(Debug, Clone, Copy)]
pub(super) struct NewWindow<'a> {
    pub(super) id: WindowId,
    pub(super) space: SpaceId,
    pub(super) app_id: &'a str,
    pub(super) title: Option<&'a str>,
}

/// What a window takes of a launch slot's width: none where its rules set one, and none where the
/// session has seen the window before, whose width is its own or the default it had.
#[derive(Debug, Clone, Copy)]
pub(super) struct SlotTaking {
    pub(super) overrides: SlotOverrides,
    pub(super) first: bool,
}

/// Writes `slot`'s home for `window` where it has none, and its width where the window has none of
/// its own, at its first sighting and where no rule sets one (`taking`): a rule's width is the
/// window's first, and a window the session has seen may be matched to a sibling's slot.
pub(super) fn take_slot_home_and_width(
    memory: &mut DisplayMemory,
    window: WindowId,
    slot: &Slot,
    taking: SlotTaking,
) {
    memory.affinity.set_window_home_if_absent(window, &slot.display_uuid);
    let width = slot.width.filter(|_| taking.first && !taking.overrides.width);
    if let Some(width) = width {
        memory.affinity.set_window_width_if_absent(&slot.display_uuid, window, width);
    }
    debug!(
        idx = window.idx.get(),
        workspace = slot.workspace_index,
        ?width,
        ?taking,
        "placing a relaunched window where it was"
    );
}
