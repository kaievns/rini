# The corner, measured

Where a tile's corner radius comes from, which curve every piece rounds on, and why each corner is
clamped. The rules are "The corner" in `README.md`.

Every piece a tile wears rounds to one corner, `DecorationLayers::corner_at`, the window's radius
clamped as `shape::radius` clamps it (`Laying::fit`): the hairline, the outline, the shadow's
silhouette, the hole its mask cuts and a clipped picture. An overview's thumb rounds them all to the
switcher's card's corner instead (`DecorationLayers::lay`, `specs/overview.md`).
Each is drawn separately, so any two that differ show as a step at the corner. A resize carries the
one corner on every piece at each of its keys, the picture's under a key of the overlay's own.

**The radius is the window server's.** A window rule's `geometry-corner-radius` goes ahead of it
for the ring and the border alone, on the tile and at rest (`Decoration::corner`,
`DecorationLayers::wear`); the hairline, the shadow and a clipped picture keep the server's, since
at rest macOS draws its own at the window's real corner. `SLSWindowIteratorGetCornerRadii` answers four corners in
points for any window a query names, on screen or not; `windows::domain::info::reported_radius`
takes the largest above 0. Read-only probes, macOS 26.7.1, 2026-10-06:

| Read | Result |
|---|---|
| 26 standard windows on screen | 16pt ×12 (Code, Slack, Ghostty, Zen, Kiro), 26pt ×9 (Finder, Messages, Outlook, Books, System Settings), 12pt ×3 (Zoom, Obsidian), 20pt ×2 (Chrome) |
| radius against size | one radius per app at every size: Ghostty 594-1148pt wide, all 16 |
| parked and other-workspace windows, off screen | answered: Ghostty 16, Zoom 1708x1065 12, Kiro CLI 16 |
| all 181 windows, every level | 0 null answers, 0 windows with unequal corners |
| 0pt answers | 89 of the level-0 windows: menu-bar strips (1728x33), popovers, AutoFill, cursor helpers, 0x0 helpers, and one off-screen "Privacy & Security" window, 723x884 |
| one query of 12 windows and the radii of each, n=300 | p50 0.049ms, p90 0.059ms, max 7.8ms (one read) |

So the flight engine reads every tile's radius in one query as it composes the flight, and an
entrance's as its first picture lands, since a window has drawn by then. A window that is not yet
pictured, or parked since before rini started, rounds right on its first flight. A 0 answer is
taken as none, not as square; every 0 seen is a window rini does not fly, though that is not
checked against the windows rini manages. Unequal corners round at the largest; none were seen,
and Core Animation takes one radius a layer. The call is resolved at run time, so a macOS without
it still launches and rounds every window at the fallback; no such macOS was tried.

**16pt where the server reports none**, `FALLBACK_RADIUS`: the commonest radius of a standard
window, 12 of 26. 10pt, which rini drew before, matches none.

**The radius measured from a picture's alpha is gone.** rini once learned a window's radius from
where each row of its first on-screen capture's top-left corner turned opaque. On `screencapture
-o` images of 5 windows that formula reads 1pt above the server every time: 17 against 16, 27
against 26, 21 against 20, and 12.8 against 12 twice. The bias is the formula's own threshold,
not the curve: both curves fit best at the server's radius plus 0.25pt (below). It saw only
windows captured on screen, learned once and was never corrected.

**The window's corner is continuous, and so is rini's.** Measured offline 2026-10-06 on macOS
26.7.1: the four corners of `screencapture -o` captures of 5 windows, each fitted to Core
Animation's own render of a layer at radius 6-40pt in 0.25pt steps, on each curve. The error is in
alpha, 0 to 1, over the corner, at the radius the window server reports:

| Window | Radius | Continuous, mean / max | Circular, mean / max |
|---|---|---|---|
| Finder | 26pt | 0.0033 / 0.357 | 0.0079 / 0.906 |
| Books (2 windows) | 26pt | 0.0031 / 0.357 | 0.0074 / 0.906 |
| Chrome | 20pt | 0.0034 / 0.294 | 0.0079 / 0.804 |
| Obsidian | 12pt | 0.0039 / 0.212 | 0.0087 / 0.565 |
| Zoom | 12pt | 0.0039 / 0.212 | 0.0087 / 0.565 |
| Ghostty | 16pt | 0.0389-0.0502 / 0.275 | 0.0434-0.0547 / 0.690 |

Both curves fit best at the reported radius plus 0.25pt. Ghostty fits both loosely; why is not
known. A circular corner misses the continuous one by at most 0.72 device pixels across the side at
26pt, 22-26pt from the corner, and 0.5 at 16pt; at the diagonal the two agree within 0.17. The
handover check keeps 16pt from each corner, so on a 26pt window it reads that gap as 0.6-1.2 levels
of `edge_in` and 0.2-0.5 of `edge_out`, and on a 16pt one as 0.07-0.08, in an offscreen model of the
check over the Books and Finder captures.

So every piece rounds on the continuous curve: the hairline and a clipped picture through the
layer's `cornerCurve`, the outline, the silhouette and the mask's hole through
`shape::rounded_rect`, which Core Graphics has no continuous form of. Its curve is UIKit's
continuous rounded rect, three cubics a corner, the one SwiftUI's continuous `RoundedRectangle`
draws. Rendered at 2x, no pixel of the silhouette or the mask's hole sits farther from Core
Animation's corner than Core Animation's own circular clip sits from Core Graphics' circular fill,
64 alpha levels (`every_path_follows_core_animation_s_corner_at_every_size`). At 8x its edge
sits within 0.25 of an 8x pixel of Core Animation's at 12-40pt, the same as the circular pair; with
the middle cubic's control at 0.18603501, as first built, it sat 2.45 off at 26pt, 0.61 device
pixels.
Where the corner's reach, 1.528665 radii (`shape::CONTINUOUS_REACH`, Core Animation's
`cornerCurveExpansionFactor`), passes half the tile's shorter side, Core Animation still draws the
corner at its full reach, the sides running back over each other, and so does
`shape::rounded_rect`: at 2x it is within the same 64 levels on tiles 1-80pt wide at 0.5-26pt
(`narrow`, and the test above). So every piece rounds on the one curve at every
size, and every path is the same 18 segments, so any two interpolate. Only a tile narrower than
80pt at 26pt, or 49pt at 16pt, passes the reach: an entrance growing from no width. There the
outline's stroke runs along the sides where they run back, flat over the corners' edge, which
dips at most 0.25pt below the side at the tile's middle: 127 pixels at 2x on a 52x400 tile at 26pt,
63 at 60pt wide, 3 at 70pt (`lid`).

**A continuous corner leaves each side 1.528665 × r from the corner**, against r for a circular
one, and reads as off the side from 1.38-1.42 × r in Core Animation's render at 2x, where a row's
alpha first drops below full: 16.5pt at 12pt, 22pt at 16pt, 28pt at 20pt, 37pt at 26pt.

**Core Animation does not clamp a radius past half a side.** A 20x100 layer at radius 16 renders
unlike the same layer at radius 10: up to 255 levels apart in alpha, on either curve, as are a
12pt-wide layer at 26 and a 0.5pt-wide one at 16 (254). So `shape::radius` clamps every corner to
half the tile's shorter side before any piece takes it, and a resize carries the picture's clamped
corner on its track rather than leaving the clip to Core Animation.

Data in `docs/data/2026-10-06-radii/`: `radii_all.m` and `radii_all.txt`, every window's radii and
the query's cost, whose `on=1` rows are the on-screen windows of the first two rows; `radii.m` and
`radii.txt`, an earlier on-screen read of 25 windows, each app at the same radius;
`radius-compare.csv`, the alpha formula against the server; `p8.swift` and `p8.txt`, the fit, whose
`win_` names are window ids (56 Ghostty, 2412 Finder, 97 Chrome, 60 Obsidian, 3851 Zoom);
`reach.swift` and `reach.txt`; `clamp.swift` and `clamp.txt`. The captures are not kept. In
`docs/data/2026-10-06-corners/`, each built with `squircle.swift`: `pathfit`, the two curves' gap
and the path against Core Animation's; `middle`, the middle cubic's control against Core
Animation's at 8x, standalone; `booksfit`, Books windows 5489 and 5299 on each curve;
`narrow`, the path where the reach passes half the side; `lid`, the outline's stroke there; `handover-model` and `sim-*.txt`, the
handover check over a capture at each curve, and `handover-offsets`, with the picture shifted.
`booksfit`'s hand-path column and the `path` rows of `sim-*.txt` drew the middle cubic at
0.18603501; the captures are gone, so they were not run again, and the `border model` rows, on Core
Animation's own curve, read the same `edge_in` and `edge_out` as the `path` rows.
