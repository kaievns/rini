# Gradients, measured

How Core Animation draws a gradient's axis and mixes its stops, offscreen, and why every gradient is
drawn through stops. The rules are "Gradients" in `README.md`.

**The axis.** niri lays a gradient over the band's own outer rect, the line through its centre at
`angle` long enough that the corners lie on the lines across its ends, every line across at right
angles in points, as CSS does (`niri:src/render_helpers/border.rs:160-175`). Core Animation takes
the start and end points in the layer's unit space and draws the lines across at right angles
there: offscreen, `(0, 0)` to `(1, 1)` on a 200×100 layer drew the top-right corner 147, the
bottom-left 145 and the centre 146
([`axis.txt`](../../../docs/data/2026-10-08-gradients/axis.txt)), the corners on one line as unit
space has it. So `Gradient::axis` stretches the axis by the size to put the lines at right angles in
points (`a_gradient_is_drawn_at_niris_angle_in_points`). In a flipped tree, as every rini tree is,
unit `(0, 0)` is the top-left, offscreen. Off a right angle the axis depends on the band's shape, so
a resize carries it on the tile's track; painted mid-resize at another angle, the band keeps the
axis of the size it was laid at until the next placement. `relative-to="workspace-view"` is refused: one gradient across
the display needs each band masked over a still gradient, riding its tile through a flight.

**The colours.** niri's shader mixes the two colours premultiplied, in `srgb`, `srgb-linear`,
`oklab` or `oklch` with its hue path, taking sRGB's curve as a 2.2 power (`colour_at`, after
`niri:src/render_helpers/shaders/border.frag`). niri leaves a colour mixed out of sRGB in `oklab` to
the GPU's power of a negative; rini clamps it into sRGB, as niri does for `oklch`. Core Animation
mixes between stops in a space of its own, unpremultiplied
([`interpolation.txt`](../../../docs/data/2026-10-08-gradients/interpolation.txt), offscreen):

| black to white, offscreen | middle |
|---|---|
| Core Animation, into an sRGB context | 147 |
| Core Animation, into a linear context, as sRGB | 147 (74 linear) |
| sRGB's values mixed | 128 |
| linear light mixed (niri's `srgb-linear`) | 186 |

147 is a 1.8 power, and the destination's space does not move it. Red at alpha 1 to blue at alpha 0
drew `(146, 33, 150)` at half alpha in the middle, the colour of the opaque gradient's middle: each
channel and the alpha mixed alone. Its ends drew sRGB red as `(255, 38, 1)`, where a solid layer of
that red drew `(255, 0, 0)`, so the offscreen renderer is no measure of a gradient's colours on
screen. So every gradient is drawn through stops, `srgb`'s too: `Gradient::stops` places them over
4096 steps, about a device pixel apart on a band around a full-screen window at 2x, each segment as
long as the colour drawn along it stays within half a level of niri's both mixed in sRGB's values
and in linear light. A 1.8 power lies between the two, and holds within one level over 42 hard
cases, black to white, primaries, complements, fades, in every space and hue path
(`the_stop_count_keeps_each_space_within_one_level`); two stops alone miss by more than 10 levels.
Those cases take 3 to 63 stops, and placing them took 1.5 to 6.1ms in a release build
([`stops.csv`](../../../docs/data/2026-10-08-gradients/stops.csv)), so the last 16 gradients' stops
are kept (`layers::drawn`). How Core Animation mixes on screen, in the window's colour space, is not
measured.
