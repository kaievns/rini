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
| **Which binding holds it** | `domain/trigger.rs` — each switcher's session keys, derived from its own binding rather than configured twice |
| **Where the rows go** | `domain/layout.rs` — the panel rect, one rect per row, the scroll that keeps the selection visible, and the hit test |
| **Whether a draw travels** | `domain/motion.rs` — how long the ring takes to move, and the one case where moving is right |
| **When the popup appears** | `domain/reveal.rs` — only once a switch has been held, never on a quick combo |
| **The popup** | `platform/panel.rs` — the `NSPanel` and its layer tree; `platform/actor.rs` — the main thread it must live on |

Focus order itself is not here. It is a fact about window focus, so it lives with the rest
of the focus tracking in `src/windows/domain/focus_order.rs` and this feature reads it.

## The shape that matters

**One row per window, never per application.** A row standing for four Slack windows cannot
take you to the third one.

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

**Three switchers, one machinery.** Every window (cmd-tab), the focused workspace, and the focused
application's windows (cmd-`) share the session, the popup, the selection and the commit. They differ
in `Scope`: which candidates are admitted and, for the application, the order. The application's
`cycle_app_windows` used to be its own code path with its own list; it is now this with a scope.

**A workspace switch spans every display.** A workspace is one context spread across displays, and
its id is the same on all of them, so the scope is the workspace rather than the display the switch
was opened on. Picking a window hidden on the other display switches that display to it, which is what
reaching any window already does.

**An application's windows rotate; everything else goes by recency.** That is the difference between
cmd-` and cmd-tab, and it is not cosmetic. By recency, quick taps toggle between the two most recent
windows and never reach a third — which is exactly the reported failure of macOS's own cmd-` with three
Ghostty windows over two rini workspaces. The rotation starts at the current window, so every list
still opens on its second entry.

**One session, several triggers.** The tap holds one live session at most, remembering which trigger
opened it: that trigger's key steps it and its modifiers hold it. Another switcher's trigger pressed
mid-switch is eaten with nothing sent, or it would reach the hotkey table and run a one-shot step in
the middle of the switch. And the most specific trigger wins, because modifiers match as "at least
these" and `Ctrl + Alt + Q` satisfies a `Ctrl + Q` binding too.

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

**The blur needed something removed, not added.** The `NSVisualEffectView` was in place and doing
nothing, because rini's own animation overlay sits ordered in at alpha 0 across the whole display at
level 18 and declared itself OPAQUE. Opaque is a promise that nothing behind the window contributes to
what is composited, and the window server keeps that promise for windows above it too — so the backdrop
this panel samples was truncated at an empty black slab. Two rounds of tinting were spent on it before
that was found; the tint was never the problem.

**Blurred, not just translucent.** An `NSVisualEffectView` with the `HUDWindow` material behind the
layer tree, blending `BehindWindow` so it samples the desktop, and a dark appearance pinned on the
window rather than inherited — the material takes its colour from the appearance, so a machine in light
mode would otherwise get a light frosted panel under a dark tint. Its state is pinned `Active` too:
rini is an Accessory application and is never frontmost, so the default `FollowsWindowActiveState` would
leave the material flat forever.

**The tint darkens with its colour, not its opacity.** This is the part that was wrong once: with the
blur in place and a 0.62 wash over it, about a tenth of the backdrop survived and the panel read as an
opaque slab — reported as the blur not working. So the wash dropped to 0.30 and moved a step down the
spine to `--n0` instead. Same intent as the native switcher, a step darker.

Four deliberate departures from the defaults, all because a switcher is not a document. It sits on
`--n0` rather than the content plane `--n2` — it floats OVER content rather than being content, and at
`--n2` too much of what was behind it read through. And the selection is RINGED rather than barred at
its left edge: the elevation law's active-row default is a soft fill plus a 2px inset bar, which is
right for a current line in a list and wrong for a focus target, and the ember's remit covers focused
borders too. And both radii are larger than the system's: the panel's because it sits beside Spotlight
and the volume HUD rather than in a document, and a tile's because the windows it holds pictures of are
themselves rounded, so a crisp-cornered tile reads as a screenshot of a window rather than as a window.
The ember has a budget of one or two appearances per screen and the selected row is the one thing here
that spends it. The tint's opacity is the single value that is still a judgement: the system has no
token for an overlay's translucency.

**The bottom inset is tighter than the top one.** Not a bug being papered over — the eye measures from
the tiles, and a caption is two thin lines of small text that read as part of the surrounding space. So
equal insets put the tile 22pt below the top edge and 54pt above the bottom one, and the panel looks
bottom-heavy however evenly the arithmetic is written. Reported twice before this was the answer.

**A tile is as wide as its window.** Every tile shares one height so the captions line up, and the
width comes from the window's own proportions — which is most of what tells two windows of the same
application apart. Clamped at both ends, because the proportions in a real strip are extreme.

**Two layers for the selection, not one.** The `--ember-soft` wash sits UNDER the tiles, where it tints
a row whose picture has not arrived; the ring sits OVER them. They cannot be one layer: under the tiles
the ring is clipped away on every side except the 4pt margin and reads as a glow, and over them the wash
would hide the picture it is marking.

**The ring travels and the strip scrolls under it.** Two transactions per draw, not one: contents with
implicit actions off, then geometry with them on. Contents can never animate — that is the cross-fade
shuffle above — and geometry animates only when the popup is already up with the same rows, because
rebuilt layers sit at the origin and animating a first draw flies the strip in from the corner.
`domain/motion.rs` is that decision, and a picture arriving mid-travel leaves the geometry alone rather
than cutting the animation short.

**Two things about the window that are not about drawing at all.** Its level is the pop-up menu level
rather than "just above rini's overlay": applications keep windows at the floating, modal and status
levels, and 21 went under all of the last one. And its collection behaviour is `FullScreenAuxiliary`,
not `FullScreenNone` — those sound like one statement and are two, the second of which means "never
shown ON a full-screen space" and made the popup invisible whenever anything was full screen.

**The popup waits for a hold.** `domain/reveal.rs` holds the first draw of a switch back for
`HOLD_TO_REVEAL`, and a switch that ends inside it is never drawn, so a quick combo steps without a
flash of panel. The actor keeps the latest draw asked for while it waits and draws that one, and builds
the window meanwhile so the first popup does not pay the 112ms either. A second draw while waiting —
which only a step produces — draws at once.

**The popup is cosmetic.** The reactor holds the list and the cursor and runs on its own thread; the
panel is a main-thread actor fed rows over a channel. A slow or missing panel delays a picture and
nothing else, because the switch is already correct on the other side.

## Reading order

`domain/candidates.rs`, then `domain/selection.rs`, then `domain/layout.rs`, then the four lines of
`domain/motion.rs`. All four are pure and fully tested without a reactor, a window server or a screen.
`platform/` after that.

## Detail

Requirements are in [`specs/switcher.md`](../../../specs/switcher.md). Cross-feature detail is
at the top of the tree: [`docs/architecture.md`](../../../docs/architecture.md).
