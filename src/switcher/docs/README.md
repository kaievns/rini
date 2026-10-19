# `switcher` — every window, by application or one by one, in the order you last used them

The strip answers "what is beside this window on this display". The switcher answers a
different question: "every window I have, wherever it is". Different question, different
feature — it reads the workspace assignments rather than any one layout tree. The every-window
switch groups windows into application stacks; the other two group nothing.

## What it owns

| | |
|---|---|
| **Who is offered** | `domain/candidates.rs` — `Candidate`, `Scope`, `switch_list`, `opening_selection` |
| **What it steps between** | `domain/entries.rs` — `Entry`, `entries`: a window, or for the every-window switch an application's stack |
| **Where the cursor is** | `domain/selection.rs` — `Selection`: stepping with wrap and clicking |
| **Which binding holds it** | `domain/trigger.rs` — each switcher's session keys, derived from its own binding rather than configured twice |
| **Where the rows go** | `domain/layout.rs` — the panel rect, one rect per row, a stack's cards inside its row, the scroll that keeps the selection visible, and the hit test |
| **Whether a draw travels** | `domain/motion.rs` — how long the ring takes to move, and the one case where moving is right |
| **When the popup appears** | `domain/reveal.rs` — only once a switch has been held, never on a quick combo |
| **The popup** | `platform/panel.rs` — the `NSPanel` and its layer tree; `platform/actor.rs` — the main thread it must live on |

Focus order itself is not here. It is a fact about window focus, so it lives with the rest
of the focus tracking in `src/windows/domain/focus_order.rs` and this feature reads it.

## The shape that matters

**One stack per application for every window, one row per window for the other two.** A row
per window made the every-window popup too long to use, so it steps between applications, as the
Dock's switcher does, with each one drawn as a stack of its windows. Reaching a particular window
is what cmd-` and the workspace switch are for, and they stay one row per window. An application is
a process, the unit `Scope::App` filters on, so cmd-` from any window of a stack offers exactly that
stack. The grouping runs after `switch_list`, on its ordered output: an application sits where its
most recent window sits, so recency is decided in one place.

**Focus order first, then a stable tail.** The window you want next is nearly always the one
you were in before this one. But a window rini has never seen focused has no place in that
order, and the enumeration it arrives in is an `FxHashMap` walk — unspecified, and different
run to run. The tail is sorted by `(space, workspace index, window id)` so two presses of the
same key walk the list instead of jumping around it.

**A switch opens on the SECOND entry.** The first is the window you are already in. Opening
there would make a quick tap do nothing, and "tap to get back to the last window" is what the
key is for.

**Minimised windows are not offered.** Minimising a window takes it off its workspace
(`handle_window_minimized` sends `WindowRemoved`), and the list is built from the workspace
assignments, so it has gone before the switcher looks. rini does not un-minimise a window it focuses
either, so offering one would land on nothing. Offering them needs both halves: a workspace kept for
the minimised window and an un-minimise on commit. The first switcher was written to offer them and
never did: its test built a minimised candidate by hand, and the reactor never reaches one.

**One owner for the selection.** Three things move the cursor — the trigger key repeating, the
arrow keys, and a mouse click. Any two of them keeping their own idea of the selected row is a
race that shows up as the popup highlighting one window while the release focuses another.

**Three switchers, one machinery.** Every window, the focused workspace, and the focused
application's windows (cmd-`) share the session, the popup, the selection and the commit. They differ
in `Scope`: which candidates are admitted and, for every window, whether they are grouped
(`Scope::stacks`). The application's cycle, now `next-window filter="app-id"`, used to be its own
code path with its own list; it is now this with a scope.

**A workspace switch spans every display.** A workspace is one context spread across displays, and
its id is the same on all of them, so the scope is the workspace rather than the display the switch
was opened on. Picking a window hidden on the other display switches that display to it, which is what
reaching any window already does.

**Everything goes by recency.** cmd-` used to rotate through an application's windows in a fixed
order, and that made it the one switcher that ordered differently. By recency, quick taps toggle
between the two most recent windows and a held switch reaches the rest. The case the rotation was
built for, macOS's own cmd-` never reaching the third of three Ghostty windows over two rini
workspaces, is answered by the scope: rini offers every one of them wherever it is, and the third is
two steps into a held switch.

**One session, several triggers.** The tap holds one live session at most, remembering which trigger
opened it: that trigger's key steps it and its modifiers hold it. Another switcher's trigger, or the
overview's, pressed mid-switch is eaten with nothing sent, or it would reach the hotkey table and
run a one-shot step in the middle of the switch. And the most specific trigger wins, because modifiers match as "at least
these" and `Ctrl+Alt+Q` satisfies a `Ctrl+Q` binding too.

**The tap holds a flag, not a list.** `src/input/domain/switch_session.rs` answers "swallow or pass,
and what do I tell the reactor" for every key event, because an active event tap sits in the event
delivery path and the window server blocks each matching event until the callback returns. The list and
the cursor live on the reactor side, where the popup will be drawn.

**Three rules there are about not stranding the user**, and each one has a test that fails when it is
removed: the modifier's release is never swallowed, a switch has a hard deadline checked against
arriving events rather than a timer, and a key the switch has no use for passes through.

**Its own window, not the animation overlay's.** The overlay is opaque black across the whole display,
only ever shown with a captured desktop behind it, sets `ignoresMouseEvents(true)`, and is shown by
fading its alpha — which a window that accepts clicks cannot do, because an alpha-0 window still
hit-tests. So the panel orders in and out instead, at ~14ms a gesture.

**An `NSPanel` with `NonactivatingPanel`.** rini runs as an Accessory application, so a plain window
taking a click would activate rini and deactivate the application being switched away from — inverting
the one thing a switcher exists to do.

**The pictures are read, never captured here.** Capture costs ~40ms plus 14.5ms per window
(measured, `src/pictures/docs/capture-research.md`), so the panel cannot take one while the
popup is opening. The panel holds a handle to the pictures service and reads its cache at every draw —
one lock and a hash lookup per card — and keeps no copy of its own, so a closed window's picture is
gone with the cache's. Only the windows drawn are read and warmed (`panel::drawn_windows`): a stack's
top card and the two behind it. Opening a switch has the reactor queue warms for them, and each one
that lands while the popup is up reaches this actor through the sink the APP wires
(`Event::Landed`), which redraws without moving anything. The switcher names `pictures` and nothing
of `animation`: `pictures`' handle, its `Picture`, and its `platform/layer.rs`, which sets the cards'
contents (`src/pictures/docs/README.md`).

**`usable`, not `get`.** The cheap capture route returns a sliver for exactly the off-screen and
hidden-workspace windows this feature exists to show, and a sliver stretched across a row is worse than
a placeholder. Age is not a reason to refuse one, though: a ten-minute-old picture beats a grey box.

**Colours are tokens, not choices.** The panel draws from the Okibi design system's resolved values:
`--n1` for the plane it sits on, `--n3` for a card with no picture yet, `--line` for the hairline,
`--n11` for captions, and an `--ember-soft` fill inside a 2px `--ember` ring for the selection.

**The blur needed something removed, not added — once.** The first blurred backing was in place and
doing nothing, because rini's own animation overlay sits ordered in at alpha 0 across the whole display
at level 18 and declared itself OPAQUE. Opaque is a promise that nothing behind the window contributes to
what is composited, and the window server keeps that promise for windows above it too — so the backdrop
this panel samples was truncated at an empty black slab. Two rounds of tinting were spent on it before
that was found; the tint was never the problem.

**Clear glass, not a frosted slab.** The backing is `NSGlassEffectView` in its `Clear` style, which
is what macOS's own switcher is made of. The `HUDWindow` visual-effect blur before it was working — shown
by putting bright stripes behind the popup — but it blurs so heavily that over a dark desktop of editors
and terminals it averages everything to one flat colour, and "the blur doesn't work" was an accurate
description of what that looks like. The native switcher barely blurs: text behind it stays faintly
readable. Clear glass keeps that shape.

**The darkening is a layer inside the glass.** The card's `BACKDROP`, `--n0`, at `BACKDROP_ALPHA`
(`src/chrome/domain/card.rs`) on the view the glass hosts; the overview darkens the bare desktop
with the same colour at its own, deeper `DESKTOP_ALPHA`, since no glass quiets it first. Not
the glass's `tintColor`, which on clear glass shifts hue and barely touches brightness — three tints
from 0.45 to 0.75 rendered almost the same. 0.40 was picked from screenshots of 0.35, 0.50 and 0.65 over
this desktop as the native switcher a step darker. The appearance is pinned dark so a light-mode
machine does not get light glass under a dark wash. The glass draws its own rim, so the panel has no
hairline of its own.

**Every card has a faint edge.** A 1px `--n11` hairline at `TILE_EDGE_ALPHA`, drawn over the picture.
Light rather than `--line`, because the cards are mostly dark windows and a hairline darker than them
disappears.

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

**A row is as wide as its top window, at its card's height.** Every row shares one height so the
captions line up, and the width comes from the top window's own proportions — which is most of what
tells two windows of the same application apart. Taken at the height the top card is drawn rather
than the row's, so a fitted picture fills its card: at the row's 163pt a maximised window's card in a
three-card stack would be 259pt wide around a 234pt picture. Clamped at both ends, because the
proportions in a real strip are extreme.

**A stack is three cards with one silhouette.** The top window and at most two behind it, each card
behind 8pt higher and 12pt narrower on each side: the card's `STACK` going up (`Stack::behind`,
`src/chrome/domain/card.rs`), which the overview's edge stacks draw toward their edges. The deepest card's
top is the row's top and the front card's bottom is the row's bottom whatever the depth, so the ring,
the hit test and the panel height are what they are for a single tile, and a stack of one is exactly
one. The inset is wider than the 11pt `CORNER`, or a card behind would show through the front
card's rounded corner; `card.rs` tests that relation. Cards behind are FILLED and cropped rather
than fitted, because only their top 8pt shows, and a fitted picture in a narrower card is centred
vertically and leaves that strip a band of placeholder. A portrait window behind a landscape one
shows a middle strip; that is the cost. Cards behind are not dimmed: the rise, the inset and the
hairline are the depth cue. The panel reuses a row's card layers between switches, so a slot past the
row's depth is hidden explicitly (`entries::cards_behind`), or a lone window would show the last
switch's cards behind it, and lets go of its picture (`draw_card_behind`): a hidden layer keeps its
contents, so the card would pin a picture the cache has since replaced for as long as no row that deep
comes back. The cards a switch draws keep theirs while the popup is hidden; see the pictures in
`specs/switcher.md`. Whether they read clearly over a dark desktop has
not been checked on screen; a `--n0` wash on the cards behind is the first thing to try if not.

**Two layers for the selection, not one.** The `--ember-soft` wash sits UNDER the tiles, where it tints
a row whose picture has not arrived; the ring sits OVER them. They cannot be one layer: under the tiles
the ring is clipped away on every side except the 4pt margin and reads as a glow, and over them the wash
would hide the picture it is marking. Both are the card's selection (`SELECTION_OUTSET`,
`SELECTION_CORNER` and `WASH` in `src/chrome/domain/card.rs`, laid by `Metrics::selection`), which
the overview's selected thumb wears too. Every card, the wash and the ring round on the continuous
curve (`round_as_card` in `platform/panel.rs`), the curve macOS draws a window's corner on, which the
overview's thumbs, their pictures and their rings must follow (`src/decorations/docs/corner.md`), so
the two draw one corner shape as well as one corner radius.

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

**A click is a commit, made where the selection lives.** The view hit-tests with `Strip::row_at`
against the strip it last drew and hands the app the WINDOW the row under the pointer is drawn for, a
stack's top one. The app forwards it to the reactor, which selects that window's row in the one
cursor, commits, and tells the tap the switch is over.
The panel accepts the first click without becoming the key window, because it is never key and a click
that only made it so would need a second one.

**Icons can be overridden per application.** `recent-windows › icons` maps a bundle identifier to
an image file, because an application that sets its icon while running — Ghostty's `macos-icon` — hands
it to the Dock alone, and every API rini can call returns the stock one. The reactor resolves the path
onto the row; the panel reads the file once, caches the result by path, and falls back to the
application's own icon with a single warning when the file is unreadable.

**The popup is cosmetic.** The reactor holds the list and the cursor and runs on its own thread; the
panel is a main-thread actor fed rows over a channel. A slow or missing panel delays a picture and
nothing else, because the switch is already correct on the other side.

## Reading order

`domain/candidates.rs`, then `domain/entries.rs`, then `domain/selection.rs`, then
`domain/layout.rs`, then the four lines of `domain/motion.rs`. All five are pure and fully tested
without a reactor, a window server or a screen.
`platform/` after that.

## Detail

Requirements are in [`specs/switcher.md`](../../../specs/switcher.md). Cross-feature detail is
at the top of the tree: [`docs/architecture.md`](../../../docs/architecture.md).
