# The window switcher

A replacement for macOS's cmd-tab. The strip answers "what is beside this window on this display"; the
switcher answers "every window I have, wherever it is".

## What is offered

- The switcher MUST offer every window rini knows about: every workspace, every strip, every display.
  It is not scoped to a display or a workspace.
- Windows MUST NOT be bundled by application. One entry per window. A row standing for four Slack
  windows cannot take you to the third one, which is the whole failing of the native switcher here.
- Minimised windows MUST be offered. macOS's own switcher shows them, and leaving them out makes a
  window you minimised unreachable by the one key whose job is reaching windows.
- A window with no workspace assignment MAY be omitted: there is nowhere to switch to for a window rini
  has not placed.
- A narrower scope — the windows of the current workspace only — MUST be available on its own binding.
  It is the same machinery with a narrower candidate set, never a second code path.

## Order

- The order MUST be most-recently-focused first. That is what makes the switcher useful: the window you
  want next is nearly always the one you were in before this one.
- A window rini has never seen focused MUST still appear, after the ones it has, ordered by space, then
  workspace position, then window id. The enumeration arrives from a hash map whose order differs run
  to run, so the tail MUST be sorted explicitly or the list reshuffles between two presses of the same
  key.
- Workspace position MUST come from the canonical workspace order, not from a workspace id — the ids
  are opaque and sort into an order the user never sees.
- A switch MUST open on the SECOND entry. The first is the window you are already in, so opening there
  would make a quick step do nothing.
- Re-focusing the window already at the front MUST change nothing. One raise produces a focus report
  per window it touches, and raising a strip window lifts the whole visible strip, so a switcher that
  reshuffled on every report would destroy the order it exists to keep.

## Stepping

- A step MUST focus the target wherever it is, switching the owning display's workspace to follow. A
  step that leaves focus on a window parked off screen reads as a dead key.
- Stepping MUST wrap at both ends, in both directions.
- The step MUST be reachable without the keyboard, through the CLI. That is the only path that still
  works when the event tap has been stood down, and rini cannot hand a redirected chord back to macOS.

> **Reported 2026-09-24.** "What I want [is] a niri style app switcher on cmd-tab instead of the macOS
> built in one... show a popup strip with a small preview of each window instead of icons (although
> small icons next to previews would be handy) and unbundle windows of every app too. The switcher
> unlike the strip will have to be a global construct showing all windows from all workspaces, strips
> and displays. I'll need a separate keybinding to cycle through the current workspace only later too.
> The behaviour will need to be similar to the native though: a quick cmd-tab cycles windows without the
> popup, a cmd hold and tab-up while holding brings up the popup, releasing cmd closes it. It should
> also accept arrow keys and mouse clicks too like a normal window switcher popup."

## Holding it open

- A quick tap MUST step without showing anything. Holding the trigger's modifier MUST keep the switch
  open so further presses move a selection, and RELEASING the modifier MUST commit it.
- Nothing MUST be focused until the commit. Focusing as the selection moves raises every window it
  passes over — a burst of Accessibility work, and a visible flicker through windows nobody asked to
  see.
- The modifier's release MUST be passed through. Swallowing it would leave every application believing
  that modifier is held forever.
- A repeat of the trigger MUST step the selection. Holding the key is how a switcher is walked.
- The arrow keys MUST move the selection while a switch is open, and Escape MUST cancel it without
  focusing anything.
- A key the switch has no use for MUST pass through untouched, and MUST NOT end the switch. Swallowing
  everything would be the more thorough modal behaviour and is also how a live session becomes a dead
  keyboard.
- A switch MUST have a hard deadline, checked against arriving events rather than kept by a timer, and
  reaching it MUST commit. The event tap can be rebuilt by a config reload, stood down for ten seconds
  by its own re-enable governor, or have its held-key cache wiped — and a session that still believed
  its modifier was held would swallow the arrow keys with nothing left to release it.
- Replacing the event tap MUST end a live session, on EVERY path that replaces it. It MUST commit rather
  than cancel: the user pressed the key meaning to go somewhere, and rini losing the keyboard underneath
  them is not a reason to pretend they did not.
- The list MUST be snapshotted when the switch opens, not rebuilt per step. It is ordered by focus, and
  focus changes the moment anything commits.
- The modifier that holds a switch open MUST be derived from the trigger binding itself. Configuring it
  separately would be a second record of one fact, free to disagree.

## The popup

- Holding the modifier MUST show a strip of the candidate windows with the selection highlighted, and
  releasing it MUST take the strip away.
- The strip MUST show every candidate. A list too long to fit MUST scroll to keep the selection visible
  rather than being capped — a cap hides exactly the tail a global switcher exists to reach.
- The panel MUST be sized to its content up to a fraction of the screen, so three windows get a small
  panel rather than an empty band.
- The panel MUST appear on the display the switch is being driven from, not always on the primary.
- The popup MUST be cosmetic. A switch works with no panel at all: the selection and the commit are
  decided before anything is drawn, so a slow or failed panel costs a picture and never a wrong window.
- The panel MUST NOT activate rini. rini runs as an Accessory application, so a window that takes a
  click would deactivate the application being switched away from — inverting the point.
- The panel MUST sit above every level an application can put a window at. Applications keep windows at
  the floating, modal and status levels, and a switcher that can be covered cannot be read.
- The panel MUST be allowed to appear over a native full-screen space. Declaring it "never full screen"
  is not the same statement as "never shown on a full-screen space", and the second one makes the popup
  impossible to see whenever any application is full screen.
- Nothing rini itself leaves on screen MAY claim to be opaque while it is invisible. The animation
  overlay stays ordered in at alpha 0 between flights, and an opaque window truncates the backdrop that
  a blurred surface above it samples — which is what stopped the switcher's blur working at all.
- A click in a gap or the padding MUST select nothing. Guessing at the nearest row selects a window the
  user did not point at.

## The pictures

- Each row MUST show a picture of its window where one is available, scaled to fit rather than cropped —
  a cropped thumbnail of a browser is a rectangle of text.
- A picture MUST NOT be captured while the switch is opening. Capture costs about 40ms plus 14.5ms per
  window, measured, and the popup has to appear at once. The panel draws what is already cached and
  nothing else.
- A row with no picture MUST still read as a row. It keeps a placeholder rather than leaving a hole.
- A picture arriving after the popup is already up MUST be drawn. The cache is asked on the open and
  answers a moment later, so the first open of a switch would otherwise stay blank.
- A picture MUST be refused if it does not cover its window. The cheap capture route returns a sliver
  for exactly the off-screen and hidden-workspace windows a switcher exists to show — measured at
  40x1081 and 1x28 — and a sliver stretched across a row is worse than no picture.
- Age MUST NOT be a reason to refuse one. A ten-minute-old picture of a window beats a grey box;
  staleness is a reason to capture again, not to withhold.
- Opening a switch MAY queue captures for the rows that have none, so the next open has them. That work
  MUST be in the background and MUST NOT touch any window.
- Each row MUST carry its application's icon as a small badge, as a cue that is readable faster than a
  thumbnail or a title.
- A tile MUST be as wide as its window is, in proportion — a full-width window reading as wide and a
  third-width column as narrow is most of what tells two windows of one application apart at a glance.
  Widths MUST be clamped: a third-width column beside a maximised window is 86pt against 260pt at the
  tile height, and 86pt has no room for an icon badge and a caption.
- Every tile MUST be the same height, so the captions line up.
- Colours MUST come from the Okibi design system's resolved tokens rather than being chosen here.
  Specifically: the panel is a floating surface, so it takes the content plane `--n2` with a 1px
  `--line` hairline and a shadow; a tile with no picture takes the raised plane `--n3`; captions take
  `--n11`; and the selected row takes an `--ember-soft` fill inside a 2px `--ember` ring. Radii are
  `--radius-card` 7px for the panel and `--radius-control` 5px inside it, because "corners stay crisp;
  only pills/circles fully round".
- The selected row MUST be ringed on all four sides rather than barred at one edge. The elevation law's
  default for an active row in a list is a soft fill plus a 2px inset bar, but a switcher row is a focus
  target rather than a current line, and the ember's remit covers focused borders as well as active
  bars. A whole outline says "this is the one" about a tile; an edge bar says "this is where I am" about
  a list.
- The panel MUST blur what is behind it rather than only tinting it. The target is macOS's own switcher,
  a step darker: a dark frosted surface you can see shapes through, not a flat wash over live windows.
- The tint over the blur MUST darken with its COLOUR rather than with its opacity, and MUST leave enough
  transparency for the blur to read. A dark material under a near-opaque dark wash is indistinguishable
  from an opaque slab, which is the failure this replaced. It takes `--n0`, two steps below the content
  plane: the panel floats OVER content rather than being content.
- The blur MUST stay active while rini is not the frontmost application. rini is an Accessory app and is
  never frontmost, so a material that follows the window's active state would never blur at all.
- The panel MUST pin a dark appearance rather than inheriting one. The material's colour comes from the
  appearance, so on a machine in light mode an inherited appearance renders a light frosted panel under
  a dark tint.
- The panel's corner radius MUST follow macOS's floating surfaces rather than the design system's card
  radius. The system tops out at 7px with "corners stay crisp", which is right for a card in a document
  and wrong for a panel that sits beside Spotlight and the volume HUD.
- A tile's corner radius MUST follow the window it is a picture of, not the system's control radius. A
  square-cornered tile reads as a screenshot of a window rather than as a window.
- The panel's inset BELOW the captions MUST be tighter than the one above the tiles. The eye measures
  from the tiles, because a tile is a bright slab and a caption is two thin lines that read as part of
  the surrounding space — so equal insets look bottom-heavy however the arithmetic is written.
- The caption band MUST be sized to its text. A caption layer draws from its top, so a taller band
  leaves the surplus underneath, and a row with no window title draws one line and leaves the second
  line's worth empty.
- The ember MUST appear once. Its budget is one or two appearances per screen, and the selected row is
  the one thing here that earns it.
- The selection's ring MUST travel to the window it is moving to, and a list long enough to scroll MUST
  scroll under it. A ring that teleports gives no cue about which direction the selection went, and a
  strip that jumps loses the sense that the selection is moving through a list rather than the list
  being replaced.
- A draw MUST travel only when the popup is ALREADY up with the same rows. Layers are rebuilt when the
  row count changes and start at the origin, so animating a first draw flies the whole strip in from
  the corner of the panel.
- A picture arriving while the selection is travelling MUST NOT move anything. Re-setting a frame
  mid-travel cuts the animation short, which reads as a stutter.
- Contents MUST NOT animate. The row layers are reused between switches and the list is
  ordered by focus, so the two most recent windows trade places from one switch to the next; a layer
  that cross-fades from its previous picture to its new one makes the strip look like it is shuffling
  itself after it has already appeared.

> **Reported 2026-09-25.** "Good radius, blur still doesn't work. Also there is some weird z-layering
> issue... when I trigger the app-switcher the popup doesn't show either, probably stuck behind something
> else." Two separate causes, neither of them the tint I had changed twice: rini's own animation overlay
> claims to be opaque while sitting invisible at alpha 0 over the whole display, and the panel declared
> `FullScreenNone` when it wanted `FullScreenAuxiliary`.
>
> The same report also asked for the ring and the scroll to be animated.

> **Reported 2026-09-25, twice.** "The padding at the bottom is visually larger than at the top, I'd
> also want to increase the corner radius on the popup itself... Also can you add blur to the background
> instead of just opacity." Then, after the caption band had been tightened and the blur added: "Bottom
> padding is still too big, the blur doesn't work, I want a similar deal to the native app switcher just
> darker, increase radius on both the thumbs and the popup itself."
>
> The second report is what found both root causes. The padding was never a caption-band problem: equal
> insets are bottom-heavy because the caption reads as space. And the blur was present but invisible,
> because the 0.62 tint left about a tenth of the backdrop showing.

> **Reported 2026-09-25.** "It's a bit too much opacity, needs to be a bit darker. For the window in
> focus just use the primary focus colour for the whole outline, not just the left border." The fill went
> from `--n2` at 0.78 to `--n1` at 0.90 — a step down the spine AND less translucent — and the left bar
> became a ring.

> **Reported 2026-09-25.** "Make the icon slightly larger, and also change the tiles width to match the
> window width. Also adjust the colours to match the [Okibi design system] specs." The one value in the
> panel that is still a judgement rather than a token is the fill's opacity: the spec has no token for
> an overlay's translucency.

> **Reported 2026-09-25.** "The two latest window icons swap visually AFTER popup becomes visible, so
> it's not synchronised properly and looks buggy." Core Animation cross-fades a `contents` change over
> about a quarter of a second by default, and the reused tiles were fading between the two windows'
> pictures. The overlay disables implicit actions everywhere it touches a layer; the panel did not.

## The trigger

- The trigger MUST be a configurable chord, never cmd-tab specifically.

  macOS cannot be made to give up cmd-tab from inside rini. The Dock's switcher is a WindowServer
  symbolic-hotkey target rather than a consumer of the keyboard event stream, so deleting the event from
  a session tap cannot retract a dispatch the WindowServer has already made. The private
  `SLSSetSymbolicHotKeyEnabled` route exists but turns every degraded state from "the key does its
  normal thing" into a dead key.

  The chord is redirected upstream instead, in the keyboard layer, which keeps the physical gesture and
  never touches the reserved combination.
- Releasing the held modifier MUST commit the selection, and the modifier's release event MUST be passed
  through untouched. Swallowing it would leave every application believing that modifier is still held.

> **Reported 2026-09-24.** "If it can't we can assign another key binding to it, I still want the
> feature." Which is what made the trigger a configuration detail rather than a blocker.

## Honest limitation

- Once the trigger chord is redirected upstream, rini CANNOT fail open to macOS's switcher: the reserved
  combination is no longer being sent. If rini is wedged, that key does nothing. This is the cost of the
  approach and MUST be stated rather than presented as graceful degradation. The CLI step is the
  mitigation.

## Where it lives

`src/switcher/domain/layout.rs` is where the rows go, `src/switcher/platform/panel.rs` the popup itself,
`src/switcher/domain/candidates.rs` is who is offered and in what order,
`src/switcher/domain/selection.rs` the cursor, `src/switcher/domain/trigger.rs` which binding holds a
switch open, and `src/input/domain/switch_session.rs` the tap's side of it. The focus order itself is a fact about focus and lives
with the rest of it, in `src/windows/domain/focus_order.rs`. Reaching a window anywhere is
`Reactor::focus_window_anywhere`. Design notes are in `src/switcher/docs/README.md`.
