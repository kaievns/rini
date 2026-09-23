# Focus and stacking order

## Who gets focus

When a workspace becomes active, focus goes to the first of these that is in that workspace:

1. a window the caller named explicitly
2. the window that had focus last time this workspace was active
3. the layout's own selected window
4. the first window the layout is showing
5. the floating window that had focus last
6. the first floating window

Every tiled candidate MUST outrank every floating one. A floating window sits on top of the strip, so
preferring one on each workspace switch buries the columns the user switched to see.

Tier 2 is what makes switching away and back feel like returning rather than arriving, and is worth
more than a "correct" choice by the layout.

## Cycling

- Cycling through a workspace's windows MUST wrap at both ends, in both directions.
- A list of one window is its own next and previous.

## Stacking order

The strip is ONE group. Everything on it stacks together, and nothing off it may sit inside it.

- The order is broken when an off-strip window has a strip window in front of it AND a strip window
  behind it. Only then is it put right, because putting it right costs one Accessibility raise per
  window on screen.
- An off-strip window in FRONT of the whole strip MUST be left alone. That is what being off the strip
  means.
- Raising a strip window MUST NOT raise the off-strip windows with it. Their order relative to each
  other is not the strip's business, and leaving them alone is what puts them behind.
- The frames written to windows are a z-ORDER, because raising is last-wins: the order a raise list is
  issued in IS the stacking it produces. Batching, filtering or grouping a raise list MUST preserve its
  order.

> **Reported 2026-09-23.** "When I open new off-strip windows like Settings, they pop up and then
> instantly move to the background, and I have to cmd-tab back into them again." The rule was "broken
> as soon as anything off the strip is in front of anything on it", generalised from a measured case
> where a Settings window sat BETWEEN two terminals. macOS raises a newly opened window, so it is
> frontmost with the whole strip behind it — which matched, and the strip was raised back over a window
> the user had just asked for. Now only a window with strip windows on both sides counts as broken.

> **Found 2026-09-23, not reported.** Across applications the raise order was reversed: the list was
> batched by `(pid, space)` through a hash map, which with 8 applications returned the batches in
> exactly reverse order. The focused window was raised first instead of last and ended up behind the
> strip it was meant to lead.

## Focus follows mouse

- MAY be enabled by configuration. When on, moving the pointer over a window focuses it.
- MUST be suppressed during a drag and during an animation, or the pointer chases windows that are
  moving under it.
- A move must not be acted on more often than the sampling interval, which is longer in low-power mode.
  Every hardware move becoming a window-server query is not acceptable.

## Crossing displays

- Moving focus horizontally off the end of a strip MAY continue onto the next display, or MAY stop,
  according to the `isolate_displays` setting.
- That setting applies to the horizontal axis ONLY. Up and down move through the workspace stack, not
  along a strip, so there is nothing to isolate and vertical navigation between displays MUST keep
  working whatever the setting says.
- Activating an application with cmd-tab MUST NOT change which display is active.

## Where it lives

`src/workspaces/domain/workspace_focus.rs` is the precedence, `src/animation/domain/motion/z_group.rs`
the stacking rule, `src/windows/domain/raise_order.rs` the raise list,
`src/windows/domain/focus.rs` the main-window tracking. Measurements are in
`src/windows/docs/multi-display-focus.md` and `src/animation/docs/capture-overlay-research.md`.
