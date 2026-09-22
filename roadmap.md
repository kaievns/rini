# Roadmap

Replaces rift's roadmap rather than editing it. Several items on that list are things this
fork explicitly does not want (more layout styles, more configuration options), so keeping
it with the name swapped would have been misleading. Rift's roadmap still applies to rift.

This is a working list for one machine and one user, ordered roughly by how much the
current behaviour annoys me.

## Known bugs

- **A bad keybinding discards the whole config.** No longer fatal: `main.rs` reports the
  error and falls back to the built-in defaults, so the WM starts and the hotkeys for
  editing the config stay reachable. But one bad command name still costs every other
  setting in the file. One bad binding should be reported and skipped, not the file
  abandoned.
- **Blank built-in display after manual window moves.** Untested since the workspace
  restructure; may already be fixed.
- **First column stays put while the strip shifts on focus.** Needs measurement with the
  external attached.
- **Some floating drags produce no `AXWindowMoved`,** so no drag session is created and the
  layout keeps reasserting the stored frame.
- **`MouseUp` can interleave wrongly with app-actor `MouseState`.** Layout changes are
  suppressed while the button is down; `MouseUp` arrives on the input thread while frame
  reports arrive per application, so a report generated during the drag can be handled
  after the release meant to unsuppress it. The symptom is a layout pass that does not
  happen, which is why it has survived: it looks like nothing. Needs a measured case
  before it can be acted on.

## Cleanup
- **Merge the two gesture tables.** `[settings.gestures]` (workspace swipe) and
  `[settings.layout.scrolling.gestures]` (column scroll) configure one event tap.

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
