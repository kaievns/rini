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

## Which window is focused

- When macOS names a window rini knows and does not manage as focused, a panel the app floats over
  the window the user is in, the focus MUST count as the app's main window. That holds for recency,
  for the window a switch treats as current, and when the app reports its new main window after the
  panel. Leaving the app ends it: a main-window change there afterwards moves nothing.

> **Reported 2026-09-29.** "the main window always ends up at the beginning and the actual call window
> is somewhere way back in the list... if i try to cmd-tilda between the main and the call windows...
> i always cmd-tilda into the call window because that's the next on the app list". Zoom's meeting
> controls, a 301×45 panel, took every focus report while Accessibility named the call window main and
> focused. The call window never entered the recency order, and cmd-` never found it as the current
> window to rotate from.

## Rapid presses

- A press MUST move focus from where the previous press put it, and a burst MUST NOT carry the strip
  backwards. macOS reports focus for rini's own raises late: raises run one at a time, and in a burst
  each waited up to 250ms for its app. So:
  - a newer focus raise MUST cancel a running raise that focuses, and drop every waiting raise that
    focuses. A window the presses moved past is never raised or focused, and the last press's window
    is focused as soon as its own raise can run;
  - a waiting raise that only restacks windows still runs, unless the newer raise covers every
    window it would raise;
  - a focus report for any window rini's raises touched or focused MUST count as rini's own echo
    until the raise manager has nothing left running or waiting, and 400ms more for the cascade,
    unless that window is the newest raise's target. If the raise manager never reports that, the
    echoes stop being believed 5s after the newest raise.
- A click on one of those windows before then focuses it without the strip following. That is the
  cost of the rule.

> **Reported 2026-09-28.** "a rapid pressing of ctrl-j/l to navigate creates back and fourth jerking
> mode". **Reported again 2026-09-29.** "now fix the rapid navigation buttons pressing animation
> confusion". Replaying one of the reported bursts as real Ctrl-J/L key events (Right ×13, then Left ×6,
> 60–570ms apart) turned the strip back three times. Each time it was a late report for a window an
> earlier press had focused. The worst came 1.2s after the last press, a 3732pt flight backwards.
> A first fix kept a replaced raise's windows as echoes for 1s. Replaying the same burst on it, the
> strip no longer turned back mid-burst, but raises were still running 2.3s after the last press and
> their reports moved it twice more. So the echo now lasts until the raises have run.
>
> **Reported 2026-09-29, after that.** "when i press rapidly several times, it seems like rini tries to
> keep executing every single instruction after i stopped tapping... we probably should skip focusing
> some windows entirely if the rapid fire moved past them". Every press's raise still ran, one at a
> time: the last queued raise timed out 2.6s after the burst, so keyboard focus reached the last
> window that late.

- A press MUST NOT wait behind a window-server round trip per window. rini reads where every window
  is from the window server, and asking one window at a time cost a burst about 130ms a press on the
  reactor: presses reached the layout up to 330ms after the key, and the flights trailed the keys.
  The frames of the windows being placed are read in one query, and each window's space at most
  once per event. The animation engine starting or ending a flight reads its windows' frames in one
  query as well: one round trip each stalled its thread for up to 237ms mid-flight, and a press
  arriving then was handled after the strip had already stopped.

- Navigating a strip MUST NOT move a window to another display or another workspace.

> **Reported 2026-09-29.** "when an exetrnal monitor is attached i frequently see windows being
> evacuated to the external screen and reattached to whatever workspace that happend to have when i
> rapidly press j/l". The Ghostty window left of this one on workspace 2 ended up tiled on the
> external display's active workspace. rini moves a window when the window server reports it in
> another display's space; what put it there is not established. A stale raise activating a parked
> window is the leading guess, and cancelling stale raises removes it. Each such move is now logged
> as "window moved to the space the window server has it on".

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

- **Each display is its own strip, and this is NOT configurable.** Moving focus horizontally off the
  end of a strip MUST stop there and bounce. It MUST NOT continue onto the next display.
- **Moving a WINDOW off the end of a strip MUST behave identically.** The window stays on its own strip
  and the view bounces. What the end of a strip means cannot depend on whether a window is coming
  along.
- This applies to the horizontal axis ONLY. Up and down move through the workspace stack, not along a
  strip, so both focus and windows MUST still cross between displays vertically.
- Activating an application with cmd-tab MUST NOT change which display is active.

> **Reported 2026-09-24.** "When an external monitor is connected and I'm moving a window in a strip and
> I reach the end of a strip on one monitor it moves the window to the next monitor, which is
> unexpected... strips should operate independently between monitors and it applies to both navigation
> and moving windows." Only focus consulted the `isolate_displays` setting; the move path called
> `next_space_for_direction` unconditionally, so the same key gave two answers depending on whether a
> window was coming along.
>
> **Reported 2026-09-24, same session.** "I don't need a config on this, I want it to be the only
> behaviour baked into the code." The setting is deleted rather than defaulted on: what it turned off —
> horizontal movement silently hopping displays mid-strip — is not a behaviour anybody wanted, and
> keeping it meant two code paths where one was never used. One rule, `boundary::strip_edge`, now
> answers both "is this an end to bounce at" and "may this cross to the next display", because they are
> the same question.

## Where it lives

`src/workspaces/domain/workspace_focus.rs` is the precedence, `src/animation/domain/motion/z_group.rs`
the stacking rule, `src/windows/domain/raise_order.rs` the raise list,
`src/windows/domain/focus.rs` the main-window tracking. Measurements are in
`src/windows/docs/multi-display-focus.md` and `src/animation/docs/capture-overlay-research.md`.
