# `chrome` — the palette and the card

rini draws chrome of its own beside the windows it manages: the bar, the switcher's popup and the
overview. This feature is the look they share, one record each, so two of them cannot drift: the
palette every colour they draw is a step on, and the card the switcher and the overview draw a
window's picture as. It is pure, names no other feature and is named by each that draws chrome, so it
sits upstream of all of them (`docs/architecture.md`, "The rules").

## What it owns

| | |
|---|---|
| `domain/palette.rs` | The Okibi 燠火 palette, dark ground: `Colour`, the neutral spine `N0` to `N11`, the one accent `EMBER` and its `EMBER_SOFT` fill, and `ERR` |
| `domain/card.rs` | The card: its `CORNER`, which the switcher's cards and the overview's thumbs round to on screen, the `GAP` between two side by side, the `RING` the selected one wears, the faint `EDGE` every card wears over its picture at `EDGE_ALPHA`, `EDGE_WIDTH` wide, and the `STACK` a run of them collapses into, each card behind a `step` further the way it goes (`Toward`) and an `inset` shorter across (`Stack::behind`) |
| `domain/card.rs`, continued | the `BACKDROP` that darkens what is behind them, the switcher's glass at `BACKDROP_ALPHA` and the overview's desktop at `DESKTOP_ALPHA`, which the overview blurs `DESKTOP_BLUR` too |
| `domain/card.rs`, continued | the selected card's selection, which the switcher's selected card and the overview's selected thumb wear: the `WASH` under it, `SELECTION_OUTSET` out and rounded at `SELECTION_CORNER`, which its ring rounds to too |

## The shape that matters

**Upstream of everything that draws it.** The bar and the switcher are the most downstream
features, and `animation` and `transitions`, which draw the overview, cannot name either. So the
look they share lives here rather than in either feature, and the palette with it: the overview's
backdrop is `N0`, as the switcher's darkening is. Only how far each darkens differs, and the
overview's blur, whose numbers sit beside the card's.

**On screen.** Every length is in points on screen. The switcher draws at one scale; the overview
draws under the strip's zoom and divides by it, so a card reads the same in both. Its thumbs' lift
shadow is divided the same way (`ShadowStyle::under`).

## Reading order

`domain/palette.rs` → `domain/card.rs`.
