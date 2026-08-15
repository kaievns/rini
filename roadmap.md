# Roadmap

Replaces rift's roadmap rather than editing it. Several items on that list are things this
fork explicitly does not want (more layout styles, more configuration options), so keeping
it with the name swapped would have been misleading. Rift's roadmap still applies to rift.

This is a working list for one machine and one user, ordered roughly by how much the
current behaviour annoys me.

## Known bugs

- **Display affinity is rewritten by observation.** `sync_display_affinity` re-homes any
  window it merely *sees* on a display, so a window that lands on the external for any
  reason is permanently re-homed there and never comes back. Measured: one window's home
  flipped built-in → external across a single move sequence. This is the root cause behind
  "windows teleport between displays", and it contradicts the comment three lines above it
  warning that overwriting the home is exactly what makes replug useless.
- **A bad keybinding stops the window manager starting.** `Config::read(...).unwrap()`
  panics on any unparseable value, so one bad command name in `config.toml` takes the whole
  WM down, with the reason buried in a launch log. One bad binding should be reported and
  skipped.
- **Blank built-in display after manual window moves.** Untested since the workspace
  restructure; may already be fixed.
- **First column stays put while the strip shifts on focus.** Needs measurement with the
  external attached.
- **Some floating drags produce no `AXWindowMoved`,** so no drag session is created and the
  layout keeps reasserting the stored frame.

## Cleanup

- **Delete the stack-line subsystem.** `collect_group_containers` now permanently returns
  empty, so roughly 1.3k lines are dead — but it is referenced from 13 files and scrolling
  still reads `stack_line.thickness()` for insets, so it needs unpicking rather than
  deleting.
- **Audit what else the single-layout prune left stranded.** The layout-mode removal was
  broad; there are likely more `LayoutMode`-shaped abstractions that now have exactly one
  case.

## Wanted

- **Per-display gaps and insets.** Currently keyed by display UUID for the SketchyBar inset
  only; the general case is unhandled.
- **Verify unplug/replug end to end with `query diagnostics`,** capturing before/after state
  rather than eyeballing it. The per-display census makes this possible now.
- **Named workspaces in the config,** with app rules targeting them by name rather than
  index. Index-based rules broke once already on the 0-based/1-based boundary.
- **Window rules for initial placement** beyond workspace assignment: column width, and
  whether a window should float.

## Explicit non-goals

Carried from the [manifesto](manifesto.md), repeated here because they are the things most
likely to look like obvious next steps:

- More layout modes. There is one, deliberately.
- A configuration GUI, or a settings surface that grows to cover every preference.
- Config or persisted-state stability guarantees.
- Upstreaming to rift. Nearly nothing here is upstreamable.
