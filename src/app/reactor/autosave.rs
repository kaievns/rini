//! The layout file written: debounced after a layout change, and at once before exit.

use rini_core::ids::DisplayUuid;
use tracing::{debug, trace};

use super::Reactor;
use super::events::command;

impl Reactor {
    /// Persist the layout after a change, at most once every AUTOSAVE_INTERVAL, so a restart or
    /// redeploy keeps every window's workspace, size and strip position.
    ///
    /// Debounced because update_layout runs on every focus change, resize and
    /// workspace switch, and serialising the whole engine plus an fsync on each one
    /// would be wasteful. The interval is short enough that at most a few seconds of
    /// arrangement is at risk, and the save is also forced on shutdown and on
    /// display reconfiguration, which are the moments that actually matter.
    pub(super) fn autosave_layout(&mut self) {
        const AUTOSAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

        let now = std::time::Instant::now();
        if let Some(last) = self.last_autosave
            && now.duration_since(last) < AUTOSAVE_INTERVAL
        {
            self.autosave_pending = true;
            return;
        }
        self.save_layout_now();
    }

    /// Write the layout file immediately, bypassing the debounce.
    pub(crate) fn save_layout_before_exit(
        &mut self,
        path: std::path::PathBuf,
    ) -> std::io::Result<()> {
        let active_space = self.active_display_space();
        command::save_layout_before_exit(
            &self.state,
            &mut self.layout_engine,
            path,
            active_space,
            self.space_state.listed_spaces().as_deref(),
        )
    }

    pub(crate) fn save_layout_now(&mut self) {
        let Some(path) = self.autosave_path.clone() else {
            return;
        };
        let active_space = self.workspace_command_space();
        // Recorded here rather than by hooks on every move and resize: the memory is a projection of
        // where the windows are now, and this is the moment it gets written down.
        let connected: Vec<DisplayUuid> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.display_uuid_owned())
            .collect();
        self.layout_engine.remember_launch_slots(
            &self.state.windows,
            &mut self.state.display_memory,
            &connected,
        );
        if let Some(listed) = self.space_state.listed_spaces() {
            self.layout_engine.forget_vacant_spaces(
                &self.state.windows,
                &self.state.display_memory,
                &listed,
            );
        }
        // autosave_current_layout, NOT save_current_layout: the latter also
        // normalizes floating-versus-tiled ownership and rewrites stored floating
        // frames. Running that on every layout change broke un-fullscreening a
        // floating window, because the frame it should return to had already been
        // overwritten. The only live state an autosave changes is the forgetting
        // above, of spaces macOS no longer lists and no window is on.
        if let Err(e) = self.layout_engine.autosave_current_layout(
            path.clone(),
            &self.state.windows,
            &self.state.display_memory,
            active_space,
        ) {
            // Not fatal: a failed autosave costs the last few seconds of
            // arrangement, not correctness of the running layout.
            debug!(error = ?e, path = %path.display(), "Autosave failed");
            return;
        }
        self.last_autosave = Some(std::time::Instant::now());
        self.autosave_pending = false;
        trace!(path = %path.display(), "Autosaved layout");
    }
}
