# Roadmap

Replaces rift's roadmap rather than editing it. Several items on that list are things this
fork explicitly does not want (more layout styles, more configuration options), so keeping
it with the name swapped would have been misleading. Rift's roadmap still applies to rift.

This is a working list for one machine and one user, ordered roughly by how much the
current behaviour annoys me.

## Known bugs

- **A bad keybinding discards the whole config.** No longer fatal: rini reports the error
  and falls back to the built-in defaults, so the WM starts and the hotkeys for editing the
  config stay reachable, and a reload that cannot be used keeps the running config. Every
  problem in the file is reported at once, with its line, bar the two kinds `specs/config.md`
  lets show only once another is fixed. But one bad action name still
  costs every other setting in the file, as it does in niri. One bad binding should be
  reported and skipped, not the file abandoned.
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
- **Merge the two gesture settings.** The file has one `gestures` block, but the workspace
  swipe (`input::settings::GestureSettings`) and the column scroll
  (`layout::settings::ScrollingGestureSettings`) still configure one event tap from two
  features.

## Wanted

- **The every-window switcher shows application stacks (asked 2026-10-01).** The cmd-caps
  popup lists every window and is too long to use. Show one stack of window pictures per
  application, with its icon, and step between the stacks, as the Dock's switcher steps
  between applications. This reverses `specs/switcher.md`'s "one entry per window", whose
  reason was reaching the third of four Slack windows; that becomes cmd-`'s job.
- **cmd-` goes by recency, like the other switchers (asked 2026-10-01).** Its fixed rotation
  is the one switcher that orders differently. By recency, quick taps toggle between the two
  most recent windows, and a third is reached by holding the popup open, as everywhere else.
- **Per-display gaps by display name.** `output "UUID"` overrides `gaps` and `struts` per
  display, by UUID only; niri also names outputs by connector or make, model and serial.
- **Verify unplug/replug end to end with `query diagnostics`,** capturing before/after state
  rather than eyeballing it. The per-display census makes this possible now.
- **Window rules for initial placement** beyond workspace assignment: column width, and
  whether a window should float.

## Explicit non-goals

Carried from the [manifesto](manifesto.md), repeated here because they are the things most
likely to look like obvious next steps:

- More layout modes. There is one, deliberately.
- A configuration GUI, or a settings surface that grows to cover every preference.
- Config or persisted-state stability guarantees.
- Upstreaming to rift. Nearly nothing here is upstreamable.
