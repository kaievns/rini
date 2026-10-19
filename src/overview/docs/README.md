# `overview` — every workspace of a display at once, while its key is held

The overview zooms one display out to every one of its workspaces, a row each, stacked top to
bottom in the stack's order. Each row is its strip at one depth, aligned on the window a switch to
that workspace focuses. The requirements are `specs/overview.md`.

This feature is the scene and nothing else: pure geometry over `rini-core`'s ids in rini-geometry's
`Overlay` space. It names no other feature but `chrome`, whose card its stacks are, and has no
`platform/`, as `layout` has none. The reactor fills its input from the layout and hands each scene
to the flight engine, and the features that draw it read its `Scene` directly: `animation`'s
overview choreography (`src/animation/domain/choreography/overview.rs`) turns it into the overlay's
score, and `transitions`' engine plays that on its own lifecycle
(`src/transitions/domain/lifecycle/overview.rs`, `src/transitions/platform/engine/overview.rs`).

## What it owns

| | |
|---|---|
| `domain/scene.rs` | `OverviewInput` in, `Scene` out (`lay_out`): one `RowScene` per workspace, its band, where its strip starts and where the display's top-left is drawn for its floating windows, the window it is aligned on, an empty one's outline, and a `SceneTile` per window with where it is drawn, where its row lays it out (`in_row`, a tiled one `apart` from its neighbours) and its `Holder`; the `Selection` the active ring is on |
| `domain/depth.rs` | The depth every row is drawn at (`depth`, between `DEEPEST` and `SHALLOWEST`) and each row's top down the display, centred when the stack fits and scrolled the least that keeps the selected row in view when it does not (`row_tops`) |
| `domain/stacks.rs` | The columns that collapse into a stack at either edge (`edge_stacks`): the walk out from the pivot column, the top card's place, the cards behind it, and how many are drawn |
| `domain/changes.rs` | What changed between two scenes (`changes`): each thumb that moved, changed holder or changed row, each row's pan, the stack's scroll, and the selection's move; the flight engine logs each scene it plays by it |
| `domain/fixture.rs` | Tests only: strips laid out as the layout lays them, on the built-in display |

## The shape that matters

**One axis.** Every row's current window is centred on the tiling area's centre x. The centre is
the one place every row can put its current window whatever its strip's scroll, and it keeps the
selection still while a press pans its row under it. A row reads its anchor off the current
window's frame, so the real strip's own scroll moves no thumb. A row whose current window floats,
or that has none, keeps its strip where it shows.

**A row is the usable frame at the scene's depth.** Tiles, floating windows and an empty row's
outline all map through the same transform, so a floating window keeps its place on the display
and a stacked column is drawn as the strip draws it. A row's `strip` is where its first column
starts: every tiled thumb sits at its window's place on the strip from there, so the overlay draws
a row as one container that a press along it moves, and no thumb in it, and the real strip's own
scroll moves neither. Each tiled thumb is that place narrowed and shortened by the card's 14pt gap
about its centre (`apart`): at depth 0.3 the strip's own gap is under a point, and a run of
terminals drew as one slab. Narrowed with its proportions kept, a short wide window took a sliver
off its height, so a column's windows stayed a slab. Narrowing rather than spreading the row keeps every place, the axis
and every pan as the strip has them. At rest the shown row's thumbs are drawn at their windows'
frames, so frame zero is still the screen.

**Stacks are the switcher's turned on their side.** Both draw `chrome`'s card stack
(`src/chrome/domain/card.rs`): the step, the inset and the three cards drawn are one record, and
only the overview's `EDGE_INSET` and `TUCK` are its own. The pivot, the current column or with none
the one nearest the axis, never stacks. Only the columns from the first that crosses an edge stack
there; the thumb beside a stack stays in its row, the pivot among them, and every row is drawn over
its stacks, so that thumb covers the top card where it reaches over it and nothing covers the
pivot. A column the top card overlapped used to join the stack as its top card, which left a gap as
wide as that column between the row and the stack (reported 2026-10-19). A top card at its edge
always reaches under the thumb beside it unless the gap between the two is wider than the inset;
then it moves in to reach `TUCK` under it. A wide top card beside a narrow thumb would reach past
that thumb's inner edge and show in the card's gap inside it, so it moves out to that edge instead,
past the display's edge if it must, and rides that thumb as the row pans rather than holding still.

**Thumbs float on a flight's lift.** Held, every thumb casts the shadow a window carried in a
flight casts (`LIFT_SHADOW`, `Node::Lift`), the selected one all of it and the rest 0.4 of it
(`SELECTED_LIFT`, `THUMB_LIFT` in `src/animation/domain/choreography/overview.rs`), so the shadow
rises and falls with the zoom on the score rather than being a layer of its own. The casters sit
under the zoom, so the engine has the overlay cast every lift under the held scene's zoom
(`Render::CastLifts`, `ShadowStyle::under`) from the open, while nothing is lifted yet, to the
lift: the shadow is a flight's on screen, where at the zoom's scale it was a fifth of it. The
switcher's cards cast none: its panel's window casts the shadow there, so there is no card shadow
to share.

**The desktop darkens on the score too.** Held, the desktop behind the thumbs is darkened by the
card's backdrop, `--n0` at 0.40, the switcher's own darkening (`BACKDROP`, `BACKDROP_ALPHA` in
`src/chrome/domain/card.rs`). It is a node of the score (`Node::Tint`, drawn by
`src/animation/platform/overlay/tint.rs`), so it darkens and clears on the zoom's curve and clock and
turns back with it, rather than on a fade of its own beside it.

**A pan is read off one window on the strip in both scenes.** The window the row is aligned on
after, else the one it was aligned on before, else any window in both. A move along the strip keeps
the moved window on the axis, so it pans nothing; the columns it passes change places beneath it.

## Reading order

`domain/depth.rs` → `domain/stacks.rs` → `domain/scene.rs` → `domain/changes.rs`.

## Known debt

**The ring thins as the overview zooms.** A thumb's ring and border are drawn at their width in the
strip's points, so the zoom out draws them thinner as it goes, down to the depth's share of their
width: a 1pt ring is 0.22pt on screen at depth 0.22. While held they are dressed again as wide on
screen as at rest (`Command::Widen`), the selected thumb's ring as wide as the card's (`accented`), and narrowed again as the zoom in begins: at rest and while
held they read at their width, but they thicken at once as the zoom out lands and thin at once as
the zoom in begins. Keeping them at their width through the zoom needs each band's width to follow
the zoom's track.

**Its thumbs keep their windows' corners, and no wash.** The switcher's card has an 11pt `CORNER`
and an `--ember-soft` wash under the selected one; the overview draws neither yet. A thumb's
corner is its window's at the zoom's scale, 3pt on screen for a 10pt corner at depth 0.3. Rounding
it to the card's means clipping every picture to it, since a picture not clipped carries its
window's own corner, and the ring would then round past it.

**A scroll reaches the window under the overview.** Every mouse button's press inside it is
swallowed (`KeyRouter::on_click`), but the tap asks for no scroll event, so a scroll scrolls the
hidden window under the pointer.

**An overview opened while a flight runs waits for it to lift** rather than taking the flight's
tiles from where they are drawn: up to the flight's length, 0.35s by default.

**Focus stops following the mouse in the reactor, not in the tap.** The reactor ignores the
pointer entering a window while the overview is open, and never asks the tap to stop following it:
the tap's switch for it, `Request::SetFocusFollowsMouseEnabled`, changes the event mask when
focus-follows-mouse is configured, and a new mask replaces the tap
(`rebuild_event_tap_mask_if_needed`), which closes the overview (`KeyRouter::tap_replaced`). A menu
opening or closing while it is open still sends that request (`update_focus_follows_mouse_state`),
and can close it that way.
