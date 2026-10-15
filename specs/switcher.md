# The window switcher

A replacement for macOS's cmd-tab. The strip answers "what is beside this window on this display"; the
switcher answers "every window I have, wherever it is".

## What is offered

- The every-window switcher MUST cover every window rini knows about: every workspace, every strip,
  every display. It is not scoped to a display or a workspace.
- The every-window switcher MUST offer one entry per application that has a window it covers, drawn
  as a stack of that application's windows, and MUST step between applications. One entry per window
  made it too long to use. Reaching one particular window of an application is the job of the
  application switcher and the workspace switcher, and both MUST stay one entry per window.
- An application MUST be one process, the unit the application switcher scopes to. Two processes of
  one bundle are two stacks.
- An application's stack MUST be made of the windows the switch offers, so an application with none
  of them gets no stack. The Dock's switcher lists an application with no window; rini's does not.

> **Asked 2026-10-01.** "i kind of dislike the all apps apps switcher. the one that is wired to
> cmd-caps at the momemnt, it's too long and unweilding. I want you to redesign it so that it would
> show stacks of app windows and let me switch between the groups. think of it as sort of like the
> the normal Dock app switcher, just instead of app icons, you'd show stacks of app windows with an
> icon." This reverses the unbundling asked for on 2026-09-24 for the every-window switcher. The
> workspace and application switchers keep it.
- A window with no workspace assignment MUST NOT be offered: there is nowhere to switch to for a
  window rini has not placed.
- A minimised window MUST NOT be offered. Minimising takes a window off its workspace, and rini does
  not un-minimise a window it focuses, so choosing one would land on nothing. The cost is that no
  switcher reaches a window you minimised. Offering one needs both halves: a workspace kept for it
  while it is minimised, and an un-minimise on commit. The first switcher's spec required minimised
  windows to be offered, and no build ever offered one.
- A narrower scope — the windows of the focused workspace only — MUST be available on its own binding.
  It is the same machinery with a narrower candidate set, never a second code path.
- The workspace scope MUST cover that workspace on EVERY display, not the display the switch was opened
  on. A workspace is one context spread across displays.
- The workspace scope MUST be a hard filter on the focused window's workspace. What another display is
  showing MUST NOT add its windows: with the laptop on ws2 and the external on ws1, a switch from ws2
  offers ws2's windows on both displays and none of ws1's.

> **Reported 2026-09-28.** "When laptop is on ws 2 and external display is on ws1, and I'm focused on
> an app in ws2 and I press cmd-tab, what I'm seeing is a merge of ws1 and ws2 windows in the popup."
> Not reproduced. In the live state at the time — laptop on `comms`, external on `main`, focus in a
> `comms` window — the popup offered `comms` alone, and a reactor test of the reported arrangement
> with windows on both displays passes. Each open now logs the focused window it resolved and the
> workspace it filtered on, so the next occurrence says which input was wrong.
- A third scope — the focused application's windows, wherever they are — MUST be the same machinery
  too. It is cmd-`, and it replaced a separate cycler with its own list rather than sitting beside it.
- With nothing focused there is no application to switch within, and the application switch MUST do
  nothing rather than guess one. A workspace switch with nothing focused MUST take the workspace on
  the display it is driven from, since one is always being looked at.

## Order

- Every switcher MUST order most-recently-focused first, and the every-window switcher MUST order
  applications by their most recently focused window. That is what makes the switcher useful: the
  window you want next is nearly always the one you were in before this one.
- The application switcher MUST go by recency like the other two. A fixed rotation made it the one
  switcher that ordered differently, which read as inconsistent. By recency a quick tap toggles
  between the two most recent windows, and a held switch reaches the rest. The case the rotation was
  for, three Ghostty windows over two rini workspaces, is answered by the scope: every one is offered
  wherever it is, and the third is two steps into a held switch.

> **Asked 2026-10-01.** "also while you're at it, switch the app windows switcher (cmd-tilda) from
> cycling to recent windows as the rest of the app-switcher popups, because cycling is incosistent
> with the others and it's  confusing as fuck."
- A window found again under a new identity for the same window-server window MUST keep its place in
  the order. It is the same window to the user, and dropping it to the back puts it behind windows
  they have not touched since.

> **Found 2026-10-15, not reported.** The focus order could carry a window across an identity change,
> and nothing asked it to: the new id was missing from the order until it was focused again.
- A window rini has never seen focused MUST still appear, after the ones it has, ordered by space, then
  workspace position, then window id. The enumeration arrives from a hash map whose order differs run
  to run, so the tail MUST be sorted explicitly or the list reshuffles between two presses of the same
  key. An application none of whose windows has been focused follows in the same way, placed by its
  first window in that tail.
- Workspace position MUST come from the canonical workspace order, not from a workspace id — the ids
  are opaque and sort into an order the user never sees.
- A switch MUST open on the SECOND entry. The first is the window, or the application, you are already
  in, so opening there would make a quick step do nothing.
- A switch with one entry MUST open no popup: there is nothing to choose between. With one
  application's windows open that is the every-window switch, and a tap on it stays in that
  application; cmd-` is the key that steps between its windows.
- Re-focusing the window already at the front MUST change nothing. One raise produces a focus report
  per window it touches, and raising a strip window lifts the whole visible strip, so a switcher that
  reshuffled on every report would destroy the order it exists to keep.

## Stacks

- A stack's windows MUST be in recency order. Its first window is drawn on top, and choosing the stack
  MUST focus that window wherever it is.
- A tap MUST step from the stack holding the focused window, whichever of its windows that is. When
  focus moves without a report, the focused window can sit behind its stack's top one.
- A stack MUST draw its top window and at most two behind it. Three cards read as a stack; more would
  thin each to a sliver.
- A stack's caption MUST be the application's name over the number of its windows the switch covers,
  `1 window` or `N windows`, counting the undrawn.

## Stepping

- A step MUST focus the target wherever it is, switching the owning display's workspace to follow. A
  step that leaves focus on a window parked off screen reads as a dead key.
- Stepping MUST wrap at both ends, in both directions.
- The step MUST be reachable without the keyboard, through the CLI. That is the only path that still
  works when the event tap has been stood down, and rini cannot hand a redirected chord back to macOS.

> **Asked 2026-09-25.** Popups for all three switchers — every window, the current workspace, and an
> application's windows (the cmd-` equivalent) — and on the workspace one: "yes to filter by workspace
> not by display, that's the expectation." The popup comes from holding a binding's modifier. A CLI call
> has no key to release, so from the CLI each of the three is a one-shot step.

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
- The popup MUST wait for the hold. It is drawn only once a switch has stayed open past a short
  threshold, and a switch that commits or cancels inside it is never drawn at all. The switch itself
  MUST NOT wait: the selection and the commit are decided the moment the keys arrive, and only the
  picture of them is held back.
- A step inside the threshold MUST draw at once. Pressing the key again is the user walking the list,
  which is as sure a sign of a hold as the modifier staying down, and stepping blind is worse than an
  early popup.

> **Reported 2026-09-26.** "The popup should not show on quick combos, it should detect whether I did a
> quick combo and released, or if I keep holding the mod button after initial tap. In the former case no
> popup, in the latter a popup." The rule above was already written; the panel drew on the open anyway.
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
- Each switcher with a binding MUST get its own trigger, and a live session MUST answer only to the
  trigger that opened it: its key steps, and its modifiers hold.
- Another switcher's trigger pressed while a switch is open MUST be swallowed and change nothing. Passed
  through, it reaches the hotkey table and runs that switcher's one-shot step mid-switch.
- When a key press satisfies more than one trigger, the one naming the most modifiers MUST win.
  Modifiers match as "at least these", so `Ctrl + Alt + Q` satisfies a `Ctrl + Q` binding too.

## The popup

- Holding the modifier MUST show a strip of the switch's entries with the selection highlighted, and
  releasing it MUST take the strip away.
- The strip MUST show every entry. A list too long to fit MUST scroll to keep the selection visible
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
- A click on a row MUST commit that row's window, a stack's top window, whatever the keyboard had
  selected, through the same cursor every other selection change uses. It MUST identify the window
  rather than the row's position, so a click can never commit a different entry from the one drawn
  under the pointer.
- The first click MUST count. The panel is never the key window, and a click that only made it key
  would need a second click to do anything.
- A click MUST NOT make the panel the key window, or the keys typed next go to the panel rather than to
  the window the click committed to.
- A switch committed by a click MUST end the tap's session, or the tap keeps swallowing the arrow keys
  until the modifier is let go and then commits a second time on that release.

> **Asked 2026-09-24, built 2026-09-28.** "It should also accept arrow keys and mouse clicks too like a
> normal window switcher popup."

## The pictures

- A row's top card MUST show a picture of its window where one is available, scaled to fit rather than
  cropped — a cropped thumbnail of a browser is a rectangle of text. A card behind it MAY be filled and
  cropped instead: only its top edge shows, and a fitted picture would leave that edge a band of
  placeholder.
- A picture MUST NOT be captured while the switch is opening. Capture costs about 40ms plus 14.5ms per
  window, measured, and the popup has to appear at once. The panel draws what is already cached and
  nothing else.
- A card with no picture MUST still read as a card. It keeps a placeholder rather than leaving a hole.
- A card the popup does not draw, behind a row with fewer windows than it has cards for, MUST NOT
  hold a picture. It is hidden, and a hidden card keeps a picture an earlier switch left on it across
  any number of switches after.
- A card the popup draws MUST NOT be cleared when the popup hides. The next show redraws it before the
  popup appears, but whether that redraw always lands before the first frame has not been checked on
  screen, and a card cleared too early opens grey.
- A picture landing after the popup is already up MUST be drawn, on the card already there. The
  pictures the open warms land a moment after it, so the first open of a switch would otherwise stay
  blank until the next step.
- A picture MUST be refused if it does not cover its window. The cheap capture route returns a sliver
  for exactly the off-screen and hidden-workspace windows a switcher exists to show — measured at
  40x1081 and 1x28 — and a sliver stretched across a row is worse than no picture.
- Age MUST NOT be a reason to refuse one. A ten-minute-old picture of a window beats a grey box;
  staleness is a reason to capture again, not to withhold.
- Opening a switch MAY queue captures for the windows it draws that have none, or whose picture no
  longer fits or has gone stale, so this open draws them as they land and the next has them. That work
  MUST be in the background and MUST NOT touch any window.
- Each row MUST carry its application's icon as a small badge, as a cue that is readable faster than a
  thumbnail or a title.
- An application's badge MUST be replaceable by an image named in the config's `recent-windows › icons`,
  per bundle identifier.
  Some applications set their icon while running, and only the Dock can see it: every API another
  process can call still returns the icon in the bundle, so the switcher would show the stock icon
  where the rest of macOS shows the custom one.
- An override image that cannot be read MUST fall back to the application's own icon, not fail the
  config. A picture is not worth refusing every other setting over.

> **Reported 2026-09-28.** "It picks up the wrong icon for Ghostty. It is picking up the default blue
> one, where the rest of macOS uses a custom orange one." Ghostty's `macos-icon = custom-style` is drawn
> at launch and handed to the Dock alone. Checked: `NSRunningApplication.icon`, the bundle's file icon,
> Ghostty's own preferences (a description of the style, not an image) and the Dock's Accessibility
> tree (no image attribute) all lack it.
- A row MUST be as wide as its top window is, in proportion, at the height that window's card is drawn,
  so the fitted picture fills its card without bands — a full-width window reading as wide and a
  third-width column as narrow is most of what tells two windows of one application apart at a glance.
  Widths MUST be clamped: a third-width column beside a maximised window is 86pt against 260pt at the
  tile height, and 86pt has no room for an icon badge and a caption.
- Every row MUST be the same height, a stack of any depth included: a stack's cards share the row's top
  and bottom edges, so the captions line up and the selection ring is one size.
- Each card behind MUST sit 8pt higher than the card in front of it and 12pt narrower on each side, so
  the stack shows as edges above the top card. The inset MUST be wider than a tile's corner radius, or
  a card behind shows through the top card's rounded corner.
- Colours MUST come from the Okibi design system's resolved tokens rather than being chosen here.
  Specifically: the panel is a floating surface, so it takes the content plane `--n2` with a 1px
  `--line` hairline and a shadow; a card with no picture takes the raised plane `--n3`; captions take
  `--n11`; and the selected row takes an `--ember-soft` fill inside a 2px `--ember` ring. Radii are
  `--radius-card` 7px for the panel and `--radius-control` 5px inside it, because "corners stay crisp;
  only pills/circles fully round".
- The selected row MUST be ringed on all four sides rather than barred at one edge. The elevation law's
  default for an active row in a list is a soft fill plus a 2px inset bar, but a switcher row is a focus
  target rather than a current line, and the ember's remit covers focused borders as well as active
  bars. A whole outline says "this is the one" about a tile; an edge bar says "this is where I am" about
  a list.
- The panel MUST be made of the same material as macOS's own switcher, a step darker: a light blur that
  keeps the shape of what is behind it. A heavy blur is not that. Over a desktop of dark editors and
  terminals a heavy blur averages everything to one flat colour, which is indistinguishable from no
  blur at all.
- The darkening MUST be done by a layer on top of the glass, in `--n0`, two steps below the content
  plane: the panel floats OVER content rather than being content. The glass's own tint shifts hue rather
  than brightness and cannot darken it.
- The blur MUST stay live while rini is not the frontmost application. rini is an Accessory app and is
  never frontmost.
- The panel MUST pin a dark appearance rather than inheriting one, so a machine in light mode does not
  get light glass under a dark layer.
- Every card, a stack's cards behind included, MUST have a faint default edge, light rather than dark,
  so a card reads as a window whatever the window looks like. The cards are mostly dark windows, and a
  hairline darker than them disappears.
- The panel's corner radius MUST follow macOS's floating surfaces rather than the design system's card
  radius. The system tops out at 7px with "corners stay crisp", which is right for a card in a document
  and wrong for a panel that sits beside Spotlight and the volume HUD.
- A card's corner radius MUST follow the window it is a picture of, not the system's control radius. A
  square-cornered card reads as a screenshot of a window rather than as a window.
- The panel's inset BELOW the captions MUST be tighter than the one above the tiles. The eye measures
  from the tiles, because a tile is a bright slab and a caption is two thin lines that read as part of
  the surrounding space — so equal insets look bottom-heavy however the arithmetic is written.
- The caption band MUST be sized to its text. A caption layer draws from its top, so a taller band
  leaves the surplus underneath, and a row with no window title draws one line and leaves the second
  line's worth empty.
- The ember MUST appear once. Its budget is one or two appearances per screen, and the selected row is
  the one thing here that earns it.
- The selection's ring MUST be drawn OVER the thumbnails. Under them it is clipped away on every side
  except the margin, which reads as a glow rather than an outline. The `--ember-soft` wash stays under,
  because a fill drawn over a tile hides the picture it is marking.
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

> **Reported 2026-09-28.** "The background blur is still broken. I think I want some faint default
> borders for the thumbs." Measured before changing anything: over bright stripes the old blur worked;
> over the real desktop it rendered as a flat slab, because it blurred heavily and the desktop was dark.
> macOS's switcher over the same desktop left text faintly readable. It is clear Liquid Glass, which is
> what the panel is now.

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

> **Reported 2026-10-06.** "stale copies definitely need deleting, no point in keeping them around".
> Fixed for the hidden cards. Read from the code: a card hidden past a row's depth kept the picture
> of the window it last showed, often the only one left once the cache had taken a newer one. Window
> captures measured 14.2MB to 29.5MB each with vmmap that day; how many hidden cards held one was not
> counted. The cards the popup draws keep theirs, for the report of 2026-09-25 above.

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

- The redirect MUST arrive as ordinary key events with the modifier genuinely held, so the hold, the
  popup and the commit on release all come from the one session the tap already runs.

  The keyboard layer sends Cmd+F17, Cmd+F18 and Cmd+F19 for the three switchers, from the same physical
  keys under every Cmd hold. Nothing on macOS is bound to those, and rini binds them like any other
  chord. The concrete mapping belongs to the keyboard layer's config, not to rini.

  Three other routes were weighed and rejected:
  - **The keyboard layer's TCP server** (kanata's `push-msg`). It is unauthenticated, and any local
    process that connects can inject key presses and load an arbitrary config — a way round the
    Input Monitoring and Accessibility permissions for every program on the machine.
  - **The keyboard layer running `rini-cli`** on press and release. It would survive Secure Input, but
    the installed kanata builds were compiled to refuse commands (checked with `--check`), and a
    command-enabled build needs its input permissions granted again.
  - **Switching the Dock's Cmd-Tab off** through the private symbolic-hotkey call, and back on under
    Secure Input. It would give an automatic fallback, at the price of a private API and a dead Cmd-Tab
    whenever rini is wedged. Declined as the less clean of two options with the same failure modes.

> **Decided 2026-09-28.** "Let's just remap all three to F*. It's the same failure modes but cleaner."

## Honest limitation

- Once the trigger chord is redirected upstream, rini CANNOT fail open to macOS's switcher: the reserved
  combination is no longer being sent. If rini is wedged, that key does nothing. This is the cost of the
  approach and MUST be stated rather than presented as graceful degradation. The CLI step is the
  mitigation.
- While any application holds Secure Input, rini's event tap receives no key events at all, so all three
  switchers are dead — and so is macOS's own Cmd-Tab, which the redirect no longer sends. Secure Input
  cuts off every observer of the key stream; it does not stop keys reaching the focused field, which is
  why remapped typing still works in a password box while the switchers do not. The CLI step still
  works, because it does not go through the key stream.

## Where it lives

`src/switcher/domain/layout.rs` is where the rows and a stack's cards go,
`src/switcher/platform/panel.rs` the popup itself, `src/switcher/domain/candidates.rs` is who is
offered and in what order, `src/switcher/domain/entries.rs` what a switch steps between, a window or an
application's stack,
`src/switcher/domain/selection.rs` the cursor, `src/switcher/domain/trigger.rs` which binding holds a
switch open, and `src/input/domain/switch_session.rs` the tap's side of it. The focus order itself is a fact about focus and lives
with the rest of it, in `src/windows/domain/focus_order.rs`. Reaching a window anywhere is
`Reactor::focus_window_anywhere`. The cards' pictures are read from the pictures service,
`src/pictures/platform/service.rs`. Design notes are in `src/switcher/docs/README.md`.
