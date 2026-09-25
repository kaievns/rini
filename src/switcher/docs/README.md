# `switcher` — every window, in the order you last used them

The strip answers "what is beside this window on this display". The switcher answers a
different question: "every window I have, wherever it is". Different question, different
feature — it reads the workspace assignments rather than any one layout tree, and it groups
nothing.

## What it owns

| | |
|---|---|
| **Who is offered** | `domain/candidates.rs` — `Candidate`, `Scope`, `switch_list`, `opening_selection` |
| **Where the cursor is** | `domain/selection.rs` — `Selection`: stepping with wrap, clicking, and following its row when the list changes under it |
| **Which binding holds it** | `domain/trigger.rs` — the session's keys, derived from the `switch_window` binding rather than configured twice |
| **Where the rows go** | `domain/layout.rs` — the panel rect, one rect per row, the scroll that keeps the selection visible, and the hit test |
| **The popup** | `platform/panel.rs` — the `NSPanel` and its layer tree; `platform/actor.rs` — the main thread it must live on |

Focus order itself is not here. It is a fact about window focus, so it lives with the rest
of the focus tracking in `src/windows/domain/focus_order.rs` and this feature reads it.

## The shape that matters

**One row per window, never per application.** A row standing for four Slack windows cannot
take you to the third one. This is the whole reason the switcher is not a wrapper over
`cycle_app_windows`, which is deliberately scoped to the focused application.

**Focus order first, then a stable tail.** The window you want next is nearly always the one
you were in before this one. But a window rini has never seen focused has no place in that
order, and the enumeration it arrives in is an `FxHashMap` walk — unspecified, and different
run to run. The tail is sorted by `(space, workspace index, window id)` so two presses of the
same key walk the list instead of jumping around it.

**A switch opens on the SECOND entry.** The first is the window you are already in. Opening
there would make a quick tap do nothing, and "tap to get back to the last window" is what the
key is for.

**Minimised windows are offered.** macOS's own switcher shows them, and leaving them out means
a window you minimised becomes unreachable by the one key whose job is reaching windows.
`WindowState.info.is_minimized` is the reliable flag; `WindowVisibility::Minimized` is not,
because the window-server visible/hidden sweeps overwrite it.

**One owner for the selection.** Three things move the cursor — the trigger key repeating, the
arrow keys, and a mouse click. Any two of them keeping their own idea of the selected row is a
race that shows up as the popup highlighting one window while the release focuses another.

**Scope is a parameter, not a second code path.** The global switcher was asked for first and a
per-workspace one is wanted later; they differ only in which candidates are admitted.

**The tap holds a flag, not a list.** `src/input/domain/switch_session.rs` answers "swallow or pass,
and what do I tell the reactor" for every key event, because an active event tap sits in the event
delivery path and the window server blocks each matching event until the callback returns. The list and
the cursor live on the reactor side, where the popup will be drawn.

**Three rules there are about not stranding the user**, and each one has a test that fails when it is
removed: the modifier's release is never swallowed, a session has a hard deadline checked against
arriving events rather than a timer, and a key the switch has no use for passes through.

**Its own window, not the animation overlay's.** The overlay is opaque black across the whole display,
only ever shown with a captured desktop behind it, sets `ignoresMouseEvents(true)`, and is shown by
fading its alpha — which a window that accepts clicks cannot do, because an alpha-0 window still
hit-tests. So the panel orders in and out instead, at ~14ms a gesture.

**An `NSPanel` with `NonactivatingPanel`.** rini runs as an Accessory application, so a plain window
taking a click would activate rini and deactivate the application being switched away from — inverting
the one thing a switcher exists to do.

**The pictures are borrowed, never captured here.** Capture costs ~40ms plus 14.5ms per window
(measured, `src/animation/docs/capture-overlay-research.md`), so the panel cannot take one while the
popup is opening. The animation engine already keeps a cache for the overlay, and
`Event::LendSnapshots` is a READ of it — a hash lookup per window. The engine answers through an
installed callback that the APP wires to this actor, so neither feature names the other. Opening a
switch also queues warms for the rows with no picture, so the next open has them.

**`usable`, not `get`.** The cheap capture route returns a sliver for exactly the off-screen and
hidden-workspace windows this feature exists to show, and a sliver stretched across a row is worse than
a placeholder. Age is not a reason to refuse one, though: a ten-minute-old picture beats a grey box.

**Colours are tokens, not choices.** The panel draws from the Okibi design system's resolved values:
`--n1` for the plane it sits on, `--n3` for a tile with no picture yet, `--line` for the hairline,
`--n11` for captions, and an `--ember-soft` fill inside a 2px `--ember` ring for the selection.

**Blurred, not just translucent.** An `NSVisualEffectView` with the `HUDWindow` material behind the
layer tree, blending `BehindWindow` so it samples the desktop. Its state is pinned `Active`: rini is an
Accessory application and is never frontmost, so the default `FollowsWindowActiveState` would leave the
material flat forever. The tint then sits over the blur and only darkens it — an opaque fill would hide
the blur it is painted on.

Three deliberate departures from the defaults, all because a switcher is not a document. It sits on
`--n1` rather than the content plane `--n2` — it floats OVER content rather than being content, and at
`--n2` too much of what was behind it read through. And the selection is RINGED rather than barred at
its left edge: the elevation law's active-row default is a soft fill plus a 2px inset bar, which is
right for a current line in a list and wrong for a focus target, and the ember's remit covers focused
borders too. And the corner radius follows macOS's floating surfaces rather than the system's
`--radius-card` 7px, which is right for a card in a document and wrong for something that sits beside
Spotlight. Radii are the card (7px) and control (5px) values, because the
system's own words are "corners stay crisp". The ember has a budget of one or two appearances per
screen and the selected row is the one thing here that spends it. The fill's opacity is the single
value that is still a judgement: the system has no token for an overlay's translucency.

**A tile is as wide as its window.** Every tile shares one height so the captions line up, and the
width comes from the window's own proportions — which is most of what tells two windows of the same
application apart. Clamped at both ends, because the proportions in a real strip are extreme.

**The popup is cosmetic.** The reactor holds the list and the cursor and runs on its own thread; the
panel is a main-thread actor fed rows over a channel. A slow or missing panel delays a picture and
nothing else, because the switch is already correct on the other side.

## Reading order

`domain/candidates.rs`, then `domain/selection.rs`, then `domain/layout.rs`. All three are pure and
fully tested without a reactor, a window server or a screen. `platform/` after that.

## Detail

Requirements are in [`specs/switcher.md`](../../../specs/switcher.md). Cross-feature detail is
at the top of the tree: [`docs/architecture.md`](../../../docs/architecture.md).
