# Space topology: one coherent snapshot, never a transient one

The spaces actor (`src/displays/platform/spaces.rs`) is the only place that turns macOS display,
space and session lifecycle signals into the `ForwardedSpaceState` the
application builds its workspace model on. The application must only do that
on top of a stable native-space picture, so the actor is deliberately
conservative:

- Sleep, display churn and lock/login transitions buffer snapshots instead of
  forwarding them. The buffered state is discarded on wake or unlock and the
  actor resamples; a snapshot taken while the login window owned the displays
  is never replayed.
- Only user spaces (`SLSSpaceGetType == 0`) may become a display's current
  space in a snapshot. Fullscreen and system/login spaces are transient native
  state and are nulled out before they can rewrite workspace mappings.
- After display churn the actor waits for two identical topology samples and a
  quiet window server (`WINDOWSERVER_QUIET_US`), then forwards one snapshot plus
  the synthesized window enter/leave deltas (`TopologyWindowDelta`) needed to
  reconcile the application with the post-churn window server state. OmniWM
  debounces at 100 ms and rescans once; rini converges on the same order of
  magnitude rather than waiting seconds.
- Window enter/leave notices arriving during a quarantine are dropped and
  counted (`QuarantineStats`), because the authoritative snapshot that follows
  supersedes them.

The failure this prevents: treating an unstable lock/wake/login snapshot as
authoritative user-space state made rini initialise fresh default workspaces
for transient spaces and later remap them onto the real desktop, which looked
like "all windows reset to workspace 1".

`ForwardedSpaceState` also carries `active_window_spaces`, which windows the
window server reports on each active space. That is why `displays` depends on
`windows` and not the other way round: the join between windows and spaces is a
window-server query over window ids, and `windows/platform/window_server.rs`
owns those reads.
