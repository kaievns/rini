# Animation smoothness: how a flight moves

How rini's flights move on the overlay: the containers that carry its rigid pieces, how a pass
arriving mid-flight merges into them, the curves, the depth bands, the edge bounce and a move's
nudge, resizes, and the score a flight evaluates to. Begun as an audit of both animation engines on
2026-08-27 and kept current since. Numbers quoted from code comments were measured when those fixes
landed; anything marked *estimated* has not been measured yet. When each step of a flight happens is
`src/transitions/docs/coordination.md`; the overlay's own measurements are
`capture-overlay-research.md` beside this file, and the pictures it draws are measured in
`src/pictures/docs/capture-research.md`.

## The overlay engine: containers carry the rigid pieces

Every overlay flight is a `FlightPlan` (`crate::animation::domain::motion::plan`): a
set of rigid groups (`RigidGroup`, keyed `GroupKey::Rigid`), a loose set (resizes and entrances), and the
floating windows. The flight's look, the config's `animations › look` as it begins (`Look` in
`src/animation/domain/choreography.rs`), composes it into the flight's score, which the flight keeps
(`src/transitions/domain/flight_score.rs`) and the overlay plays: Slide, the default look
(`src/animation/domain/choreography/slide.rs`). Frame zero is its pose, every group with a drawn
member a container at the origin, which the overlay builds its tree from (`TileOverlay::pose` in
`overlay.rs`): each group is one `CALayer` container, the floating one and its twin under the
overlay's root and every other under the strip layer, whose `transform` is the strip's zoom, at rest
unless a look zooms. A container's
`position` is the only animated translation its members get; the tiles inside sit at
frames in its own points (`Group` in `crates/rini-geometry/docs/README.md`), the overlay's less its
position, and never move on their own. Resizes and entrances
are loose tiles in `GroupKey::Loose`; floating windows sit in the `Floating`
container. As the motion starts the look composes the whole plan as the score's edits, and the
renderer plays them (`TileOverlay::play`) in ONE `CATransaction`, the one its drain is played in
(`src/animation/platform/renderer.rs`): one position animation per moving
container, one resize per loose tile, one timebase, one curve (`MOTION_CURVE`, or the ease-in-out
while a move steps the strip; see "The move flight"). Model layers jump to their
destinations; the animations carry the presentation and are removed on
completion. The tick loop paces only the coordinator (frame zero's wait,
the coalesce window, the hold, the final capture, the lift); nothing is drawn
on ticks. Every drain's transaction is flushed to the render server at
once: an explicit commit nested in the run loop's implicit transaction is
only sent when that pass of the main thread's loop ends, after whatever
else runs in it. Sent late, a flight skipped its first frames. Every
animation is installed from a track (`src/animation/domain/motion/score.rs`)
and carries the track's explicit `beginTime`, which the engine stamps on the
media clock the renderer read (`MediaClock`) at the instant the coordinator
was driven, so the render server's clock and the coordinator's agree, and a
batch played after it was sent plays from where the score already is.

The model has been round the loop twice. The original manual tick loop
(60Hz `RepeatingTimer` posting into the actor queue) was not vsync-aligned
and dropped drawn frames. A dedicated canvas layer then carried switches
and pans as one animated property, rigid by construction. The canvas was
dissolved into per-tile animations committed in one transaction: that
bought one machinery for every movement, and continuity across a chained
switch and slide, because each tile bent from its own presented position.
It cost rigidity on every merge. Each tile was retargeted from its own
presented position on its own fresh clock, and a pass merging mid-flight
(a pan 56ms after an open, a resize during a pan) sent tiles of one strip
off on slightly different legs. The user saw the strip teleport. Containers
keep the continuity (a container carries on from where its own leg has it;
see "One flight however many presses") and restore rigidity structurally:
members of a group cannot drift because only the group moves.

How a layout pass and a strip movement start a flight, and which pieces each makes, is "Layout
changes and strip movements" in `src/transitions/docs/coordination.md`.

**Mid-flight passes** go through `merge_plans` (pure, tested), which retargets containers, not
tiles. A rigid member of the incoming pass votes for its group's new position (`p = to - rel`); the
largest cluster keeps the container, which carries on toward the new destination under the same
animation key (`GROUP_ANIMATION_KEY`, `"rini.group.move"`); see "One flight however many presses"
below. Members voting elsewhere are reparented at the frame they are drawn at, into a group whose
remaining travel matches theirs and whose movement goes as fast as the one they leave, within 60pt/s
on each axis (half a point over a 120Hz frame), or into a new one. The votes settle one group at a
time, so a member lands only in a group already settled or one no vote of the pass names: a group
settled later may be sent elsewhere and carry the member with it. Seed 91's pass after a switch kept
a window where it was sent and put it in a group with as far left at the same speed, which the pass
then sent 150pt left and 100pt down; pinned by
`a_member_kept_where_it_was_sent_lands_in_no_container_a_later_vote_moves`. Over seeds 0-999 of the
bend contract's flights that failed seeds 91, 378, 615 and 953 under Slide, Zoom and Probe and the
first three under Linear, each at the pass after a second switch, and fails none now
(`every_look_keeps_the_bend_contract_over_a_thousand_seeds`, 57s in release). Settling every group
before any member leaves would also let a member land in a group settled after its own, where it
opens one now, but changes Slide's output (`SLIDE_SCORES`): 9 members of seed 163's Slide flights
would land so. Matching the remaining travel alone, a still container's member took a group with
151pt left at 2821pt/s and started at that speed, and a member drawn where the pass sends it took
the still group and stopped from 2211pt/s; pinned by
`a_window_leaving_its_container_lands_only_in_one_going_as_fast`, and a group a member opens takes
the speed of the one it left, pinned by
`a_container_a_member_opens_goes_as_fast_as_the_one_it_left`. Over seeds 0-999 of the speed
contract's flights, the seeds failing went from 30 to 0 under Slide, 101 to 0 under Linear and 67 to
38 under Zoom. Each of Zoom's 38 was a window speeding up by 5-12% where a pass bringing an arc
lands mid-arc, 3246 to 3538pt/s in one flight: the pass's own arc is dropped, but Zoom still runs
its springs over 0.35s times `ZOOM_STRETCH`, 0.257s, at `spring_omega` 41.7 where 0.35s gives 30.6.
Such a spring starts at the drawn speed and changes it fastest over its first frame, and the
contract sized its allowance from the new motion's second. Kai kept the pace ("Arriving mid-flight"
in `specs/animation.md`), so under Zoom, across a pass bringing an arc, the contract also allows a
laid-out edge the change a spring over the pass's length stretched by `ZOOM_STRETCH` makes over its
first frame, let go where the edge is drawn at the speed it is drawn moving, and only in the way
that spring makes it (`stiffer_on_an_arc`), and only to a window whose container the pass carries on
that spring, as the contract finds it drawn over its first two steps (`springs`): a container Zoom
folds into the arc playing goes on `EASE_IN_OUT`, one it opens for a newcomer on `MOTION_CURVE`, and
one it does not bend keeps what it had; a window riding any of them is allowed none, 2,272 of the
7,333 windows across Zoom's passes bringing an arc over the bend contracts' seeds (`BEND_SEEDS`).
Seed 60's window gained 292pt/s; that spring gains 304pt/s, one over 0.35s 10pt/s, and the edge's
allowance went from 286 to 430pt/s; pinned by
`a_bend_may_change_speed_as_far_as_a_spring_as_stiff_does_in_its_first_step`, which also reads a
bend slowing as far as a jump, and by
`zoom_bends_a_pass_bringing_an_arc_within_its_stretched_spring`. Every other look, every pass
bringing no arc and the zoom's own read keep the allowance from before. Given to every window across
such a pass, the allowance passed Zoom with each fold sped nine tenths of the way onto that spring
on every one of seeds 0-999; held to the windows riding it, it rejects that on 19, seed 82 run 188's
w1 going from 2322 to 2464pt/s against 137 allowed
(`the_speed_contract_rejects_a_fold_sped_up_toward_the_stiffer_spring`). Allowing every look's every
pass a spring over the plain or stretched length grew the allowance of Linear, which bends on no
spring, 6.9 times on average and 48 times at most over seeds 163 and 2, against 1.04 times for
Slide. Zoom now fails none; `every_look_keeps_the_speed_contract_over_a_thousand_seeds` counts every
look's, ignored because it runs 141s in release. A member the pass sends off the viewport does not
vote while its container moves (`rides_out`): it has no visible destination, and on a switch the
layout pass 16ms later parked the departing row's windows, each of which opened a sideways group and
swept to the park's edge while the row travelled vertically (the zig-zag; log 1:07:36, "member
reparented" to (1727,1065) and (-1719,1076)). A member of a still container has no motion to ride
and leaves on its own vector. A rigid member the pass now resizes goes loose (`GroupKey::Loose`) at
its presented frame. A newcomer joins a group travelling as it does in this pass or opens one; joins
are resolved after the votes, because the votes change how far each group travels. A pan adds its
travel to every group and every loose strip tile and changes no membership; members the pan does not
compose (no usable picture) ride their group all the same. A move under a pan is the one exception:
the members of a group that does not travel with the pan (the columns the move swapped that cross
it) vote as in an ordinary pass, wherever their destination is, and the rest of each group they sit
in holds the place the pan gave it. The merge reads two positions of every container and how fast
its movement goes (`Presented`), evaluated from the flight's score (`Score::presented`): where it is
drawn, any bounce or nudge riding it included, and where its movement has it. A member leaving a
container for one of its own starts from the first, so it stays where it is drawn and still lands on
its destination; pinned by `a_member_leaving_a_nudged_container_leaves_from_where_it_is_drawn`.
Remaining travel of a member leaving its container is judged against the second. A newcomer is
judged against neither, but against where the last pass sent each container (the plan's model
positions): every frame a pass carries is a place on the strip as the last pass left it, which the
reactor builds from where that pass sends every window. A newcomer rides a group whose travel from
there matches its own, placed in it where that pass left it, so it starts beside the windows it sits
beside wherever they are drawn, and an accent riding the group carries it too; of several such
groups it takes the one holding the window nearest it. Judged against where the leg has it, a column
a second pan brought mid-leg matched no group's remaining travel, opened its own and started where
the first pan sends the strip, short of the strip by the travel left: of two 863pt pans, the second
merged 7ms into the first started its column 759pt short, 20ms in 591pt (measured with the test that
pins it against that rule; not seen on screen); pinned by
`a_column_a_pan_brings_in_mid_flight_starts_beside_the_strip` and, for every look, by
`every_look_bends_a_flight_without_a_jump`. A floating window a switch brings is placed in the
floating container where the last pass left it, the same way. A move's step rides every strip
container, so a still window joining mid-step takes the still piece and steps with the strip; pinned
by `a_still_window_joining_mid_step_steps_with_the_strip`. Before the flight moves, a pass is folded
in by the same function, every container drawn where frame zero installs it and its leg taken to be
where the last pass sends it: nothing has moved, and the pass's frames are places on the strip
there. That is the look's rule too (`Slide::fold`), and frame zero is posed again from the folded
plan. Every look's fold sends each window to the frame the pass sends it to, size as well as place,
and starts each window it leaves in its container, or on its kind of path of its own, where it
started; frame zero draws each window where the folded plan starts it, so those windows hold still.
A window the fold moves to another container or kind of path may start elsewhere: over the
contract's 200 runs, 8 of 258 such windows moved at frame zero under Slide, the farthest 1199pt, and
none of 2552 that kept theirs (measured with a probe over the contract's seed; not checked on
screen). The pass's own start frames bind no start: they are places on the strip as the last pass
left it. Pinned by `every_look_folds_each_pass_before_the_motion`. What the flight does about a
merge, the clock, the frames already sent and the entrances still reserved, is "Mid-flight passes"
in `src/transitions/docs/coordination.md`.

**One flight however many presses.** A retargeted container used to restart `MOTION_CURVE` from its
presented position. That curve leaves at 6.25x its average speed and is 97% home by half time, so
under a burst the strip had nearly stopped by each press and the restart kicked it: 150ms into a
column's travel it moves at about 890pt/s, and the restart put it at 15,375pt/s in one frame.
Reported as a series of jerky movements. Now each container keeps its current leg (`Leg` in
`src/animation/domain/motion/glide.rs`), and a new destination starts a critically damped spring
(`Leg::toward`) from where the flight's score draws its movement (`Score::along`) and as fast as its
leg is going, which Slide keeps. The spring's stiffness follows the movement's length, 0.35s times
`animations › slowdown`: 97% home from rest at half the duration, where the curve is, so a chained
leg lands like a fresh one. Core Animation cannot take a spring with a starting velocity in two
dimensions as one timing function, so the leg is handed over as keyframes sampled from `Leg` every
1/120s (`Leg::track`), which Core Animation joins with straight lines. The spring starts on that
polyline, not on `Leg::position_at`: from rest the first line cuts the corner by up to 10.8pt in its
first 1/120s (`a_spring_plays_as_the_polyline_of_its_samples`), and a press landing in that frame
used to jump the strip by the difference. The first leg of a flight is still the curve, on which the
two agree. Starting from the movement rather than what is drawn also keeps an edge bounce out of it:
the drawn position includes the bounce's additive offset, and a leg begun there carried the bounce
twice. A container with no leg (a still one, or one placed outright) starts from its model position
at rest for the same reason: it used to start from its presented position, which under a move's
nudge is a third of the display out. A container a merge opens for a member leaving another carries
on the one it left: a spring from where the member is drawn, as fast as the old container's leg is
going (`Leg::toward` given that leg), even when it is drawn where the pass sends it: the spring
overshoots and comes back, where with no leg it stopped dead. It used to start the curve from rest,
and the column a second move passes left the strip at 12x the speed it had a frame before, 5,700 to
53,000pt/s in a full-width burst; pinned by `the_column_a_second_move_passes_keeps_its_speed`. The
flight's clock covers the score: a merge that moves anything restarts it at the pass's length or
what is left of the score, whichever is longer (`FlightScore::clock`), so the longest spring and a
bounce still playing are both covered. It used to restart at the longest leg the merge made, and
could lift before a bounce came home.

Measured 2026-09-29 by tracing where each container is drawn every tick
(`TileOverlay::trace`, enabled with `rini::animation::trace=trace` in `RUST_LOG`;
it logs the presentation beside where the replica draws the container, and the gap)
through one replayed burst of real Ctrl-J/L key events, Right x13 then Left x6,
60-570ms apart. Speed is only compared within a flight: each new flight re-bases
its container, so positions jump between flights without anything moving.

```
change of speed in one tick   old retarget   spring
largest                        28,338pt/s    9,239pt/s
p99                            20,010pt/s    8,328pt/s
```

A retarget now accelerates over about 30ms from the speed it was at (210, 4163,
8489, 9433pt/s on consecutive ticks) instead of jumping. What still reads as
separate pulses at 300-570ms between presses is separate flights: the first
leg is 97% home by half time, so the strip is at rest before the next press.

Every look keeps that speed for a window riding a container
(`every_look_bends_a_flight_at_the_speed_it_moves`): across a pass, each edge's speed over the
1/120s step either side, as the window is laid out on the strip, changes by no more than the motions
themselves change it over the steps around it, the motion after over its second step or, under Zoom
across a pass bringing an arc on a container riding a spring over the stretched length, as that
spring changes it over its first and the same way, whichever is more, and a half point over a step;
the strip's zoom, its scale and shift, is held to the same rule on its own but for the spring, which
it does not ride, a strip the score holds no zoom for read at rest, as 429 of Zoom's 995 bends over
seeds 163 and 2 find it and 73 then start it zooming. Drawn as Core Animation's polyline a spring
changes its speed at every sample, so the bend's own change, up to 4,385pt/s under Slide over seed
163's flights, is held against the motion's, at most 0.71 of it over seeds 163 and 2, 0.84 under
Zoom; a look restarting each container on `MOTION_CURVE` from where it is drawn changes it by up to
9.3 times the motion's, and jumps on 11,382 of the 31,272 edges seeds 163 and 2 read, and one
sending the zoom home from rest where it is drawn, or starting a strip with no zoom zooming at
speed, is caught on the zoom
(`the_speed_contract_rejects_a_look_that_restarts_the_motion_or_the_zoom_at_a_pass`), and Linear,
before it carried its speed, by up to 79 times. Read drawn, a window through a zoom moves at both
speeds, and the two turning opposite ways cancel the allowance though neither jumps: under Zoom,
over seed 163, a spring let go at its container's own speed while the strip zoomed out changed an
edge drawn 1,065pt left of the centre from 429 to -227pt/s against 633 allowed. In its run 25 a
window at rest on a strip at rest starts left at -576pt/s laid out, within its 755 allowed, while a
shallow zoom starts from rest at -0.15/s, within its 0.29/s, and draws it back towards the centre:
drawn, the edge goes from 0 to -297pt/s, and the window's speeding up and the zoom's pulling harder
each frame leave an allowance of 206. **Slide's loose tiles still restart:** a resize, entrance or
floating window a pass bends starts `MOTION_CURVE` again from where it is drawn (`tile_leg`), so its
speed jumps, against "Arriving mid-flight" in `specs/animation.md`. Over seed 163's flights a
resizing tile's changed by a median 3,408pt/s and up to 16,676pt/s, and one a pass made resize out
of a moving container's by a median 5,111pt/s (measured with a probe not committed; not seen on
screen), so the contract holds only windows riding a container.

Zoom bends what the view carries with each container (`src/animation/domain/choreography/zoom.rs`).
A pan the way the view is going, along the strip or, a switch, up or down the stack, folds into the
arc playing: each container's movement as it plays, the floating one's with it under a switch,
shifted on by the press's travel on `EASE_IN_OUT` over what is left of the arc, so it leaves at the
speed it had and lands with the zoom. A diagonal pan does not fold. A switch folded into the
shortest share left, a quarter of a 0.257s arc, carries 1,109pt in 0.064s, as a press along the
strip folded in there carries its column's width; `FOLDED_PAN_MIN` is the knob for both. A press
back turns on a spring let go at the speed the score draws, and carries on 10.7, 16.1, 27.6 and
28.6pt for presses 0.02, 0.04, 0.067 and 0.1s into the 0.257s arc of a half-width focus press, where
folding it in would carry on 590 to 852pt before turning
(`a_press_back_turns_the_strip_on_a_spring`). The strips Zoom draws above and below the one a flight
moves ride still containers of their own (`RigidGroup::beside`), so neither the arc nor a fold nor a
spring moves them: a pan moves them only as far as the view goes up and down, and every pan the
reactor sends names them standing where they are, so they vote themselves where they are and get no
track (`a_pan_merged_leaves_every_row_where_it_is`). A window of the strip never joins one: the bend
contract's flights drew a strip window brought still by a pan into a row's container, and a layout
pass moving that window later dragged the row sideways 350pt
(`a_row_shares_no_container_with_the_strip_and_moves_only_up_and_down`). Half the flights the bend
and fold contracts draw carry one or two such strips, each sent nowhere by any pass, and after their
seeded passes, so those streams stay as they were, up to two switches onto one of them (`a_switch`),
each a pan up or down by the pitch naming that strip the strip's and the strip a strip beside, half
of them bringing a switch's arc, then a pass. Over the bend contract's flights 11 switches folded
into an arc playing and 9 turned on a spring (measured with a probe not committed). A member a pan
names of another strip than its container holds votes, and the container comes to hold that strip:
before, a switch merged into a pan drawing rows left the destination where the pan moved its still
container, 24pt low by the gap the switch's pitch lacked
(`a_switch_merged_into_a_flight_drawing_rows_lands_the_destination_where_the_switch_sends_it`), and
a pan after a switch left the destination standing, its windows still in the container of the strip
beside they had been, which both contracts now catch.

The look reads every container's two positions once, from the flight's score, for `merge_plans`,
and turns the delta into the score's edits, which the overlay plays in one transaction: reparents,
container animations, joins, loose retargets, and the `Bank` the engine appends. What rides where
and where each node ends is every look's alike (`Merged::bend` in
`src/animation/domain/choreography.rs`); the look gives each node's movement (`Bending`), Slide's
the springs and legs above.
Without this a pan merging 56ms after an open (log 3:27:20, 22 tiles
scrolled 574pt) left the newcomer at its pre-pan slot: a tear between the
active window and its neighbours, then a pop at lift. Cross-container z is a
tie rule, not a guarantee: strip windows do not overlap at rest, so overlap
between two moving groups is transient, and the group holding focus is drawn
first.

Depth is banded (`stack` in `animation/domain/motion/z_group.rs`) at the
container level (`band_plan`, played as the score's `Bank`), in three bands with the strip always
the middle one: `Lifted` in front of it, `Behind` behind it. Only one set of
off-strip windows comes forward with a focus, and that set is the focused
window's APPLICATION — a zoom call is a meeting window and its controls — plus
any off-strip window the server already has in front of the strip, which is
where macOS leaves it. Every other off-strip window stays behind. With focus on
the strip nothing is lifted, so the strip leads at zero and the floating
container is one `GROUP_STRIDE` behind; with focus off it, the strip is one
stride back and the floating container two.

The floating container therefore has a twin (the overlay's `lifted`) in front of
the strip, and a `Bank` moves each floating tile into whichever of the two its
band names. A twin rather than a second plan group: every floating window rides
one translation, so the twin is opened, moved, animated and dropped with
the floating container (`layers_of`) and a tile moved between them lands where
it was. It has to be a separate layer at all because tiles are children of their
container: a strip container's order against the floating container and its twin
is the strip layer's `zPosition`, and the floating container's is its own, so
one floating container cannot put one application in front of the strip and
another behind it. Found drawing zoom in
front of the strip for the length of a switch to 1Password and dropping it
behind on landing ("Which windows come forward together", `specs/animation.md`).

Strip containers sit a quarter step apart in `Banding.group_order`. Each tile
sits at its within-band depth inside its container (`Banding.within`; a
shadow half a step behind its picture). `container_z - within` is `-depth`, so the drawn order is
what `restack` computes for `tile.depth` and what the reactor's regroup
(`regroup_tiled`, "Real order" in `capture-overlay-research.md`) puts on the
real screen at lift. Core Animation sorts `zPosition` among siblings only,
so a floating tile can never land between two strip tiles whatever its own
depth: that is the structural fix for a floating tile drawn between two strip
tiles. Within a band the
window server's order is kept; a window the server did not report goes to
the back of its own band. An entrance carries `server_order: Some(0)`
because a window is raised on open.

**Holes.** A window wholly under the overlay is not composited, so a window
that repaints for focus under it shows the change only at the lift ("The
handover, measured" in `capture-overlay-research.md`). `TileOverlay::set_holes`
punches a 1pt hole over each window the caller names, a point in from its left
edge and halfway down its final frame (`hole_in` in
`src/animation/domain/motion/holes.rs`): one even-odd `CAShapeLayer` mask on
the content view's layer, the view's bounds and every hole, with the window's
background clear while it is up: the window's, so `paint_background` sets it
once the renderer has flushed the mask, outside the transaction like the
alpha. Each call replaces the holes; `clear_holes`, which a lift and a new
display frame call, removes the mask, and the black goes back after the flush.
Pinned by `the_hole_is_inside_the_window_and_off_its_corners` and
`the_mask_leaves_out_exactly_the_holes`. The mask is in the view layer's
coordinates, flipped as the canvas is; where a hole lands on screen was not
measured on a window shorter than the display. Which windows get holes, and
when, is the coordinator's (`src/transitions/domain/lifecycle.rs`): every window
it watches end where its hole lands on the display, from the show to the lift: one
ending wholly on screen, or a grow ending cut by the display's right edge, never
one cut by its left edge, whose hole a point in from that edge is off the display. A window wearing a
border, or its focus ring where the flight ends, gets a second hole, over the
bands rini draws around it at rest: a point square, or their width where that is
narrower, halfway across their left edge and halfway down (`band_hole_in`, pinned
by `the_band_hole_is_outside_the_window_and_inside_its_band`), where its tile and
the rest window draw the same pixels. The engine gives each end how far its bands
reach, in device pixels (`shape::outset`,
`a_decorated_window_is_holed_in_its_band_as_well`).
That a rest window wholly under the overlay is held back as a window is, and so
needs the hole, is inferred, not measured.

**Corners of a baked picture and of the stand-in.** A tile cut from a composite
of the desktop carries the wallpaper in the window's rounded corners, where the
window itself is transparent, and the stand-in, one flat pixel, has no corners
at all. The overlay clips such a tile's picture layer, crop pieces included, to
the corner its hairline rounds to (`cornerRadius`, laid at every placement, with
`masksToBounds`), whose corner a resize's track carries with the hairline's, so
the clip stays clamped to the tile as it is (`src/decorations/docs/corner.md`); the shadow caster and the hairline are
never clipped (`a_tiles_picture_clips_to_its_corners_and_its_shadow_does_not`).
The decision is in `docs/pictures-motion-transitions.md`. The pictures service
bakes them (`capture_on_screen`). The flight engine marks each tile
`OverlayTile::clipped` as it builds it and as a picture comes onto it, by the
picture's own route (`Route::clips`): the stand-in, or a picture from SkyLight's
baked call. What the service knows of the window now does not decide it: a
window first seen inactive is baked once and then pictured alone. The overlay
applies it at every dressing and swap, so a tile riding into a moving flight is
clipped too (`a_tile_ridden_in_clipped_wears_its_corners`).

**Tiles snap to the real frame.** A strip pans by a fractional vector and the
window server rounds each window on its own, so a tile of a rigid group can end
a point from its window: 861 against 860 wide ("The handover, measured" in
`capture-overlay-research.md`). A tile snaps when it ends more than a quarter
point and at most 20pt from its window at any edge (`snap_to`, with the pictures
service's landing slack `END_SLACK`): under a quarter point nothing shows at the
lift, and past 20pt the window has not landed where its tile ends. The caller
decides with `snap_to`, once per window per generation and only once the motion
has started: before it, the container stands at frame zero and the start would
move the tile back. A window pictured where it ends before the start, or before a
merge that can put its tile back on its model, is snapped again then, since a
delivery comes only with a changed print. `tile::snap` (`src/animation/domain/motion/tile.rs`) makes
the snap the score's edits, which the flight folds into its score and the overlay plays, without
checking again. A tile riding its container moves inside it at once, while the container
carries it on. A tile on a track of its own is given the same track ending on
the real frame (`Track::ending_at`), its begin, start and curve kept, so it
lands there with its movement instead of popping at the lift
(`a_tile_on_its_own_leg_snaps_on_the_same_timing`), drawn stretched or cropped as it was; once the
track has played only its model moves. Shadow, mask, crop pieces and hairline follow the tile.
The caller is the coordinator (`src/transitions/domain/lifecycle.rs`); the
flight engine carries its snaps out.

Mechanics worth remembering:

- **The curve.** One cubic Bezier, `MOTION_CURVE` `(0.16, 1, 0.3, 1)`, an
  exponential ease-out. A `Leg` and the score evaluate it by
  solving the Bezier for time (`CubicBezier::ease`, Newton then bisection)
  and Core Animation gets the same four control points (`timing_function`),
  so where a merge bends a container from and what is drawn agree; pinned by
  `a_leg_the_score_and_the_render_server_run_one_curve`. A flight that steps flies
  its containers on CSS ease-in-out (`EASE_IN_OUT`) instead, the same way:
  `Leg::Curve` carries its curve (see "The move flight"). Progress at quarter,
  half and 70% of the duration: 0.87, 0.97, 0.995. Ease-out cubic
  (`1 - (1-t)^3`, the thirds Bezier `(1/3, 1, 2/3, 1)`, for which x(t) is
  the identity) ran before it and read as sluggish at 350ms: 0.58, 0.875,
  0.973 at the same marks, so the whole second half of every flight was a
  crawl through the last 12.5% of the distance. The duration was right; the
  tail was the problem. Pinned by `the_motion_is_nearly_home_by_half_time`.
- `CAAnimation` treats a zero duration as "use the default 0.25s", so zero
  durations bypass the animation and draw the final frame directly.
- `NSValue::valueWithPoint` (the from/to carrier) requires the
  `NSGeometry` + `objc2-core-foundation` features of `objc2-foundation`.
- **Whole points.** Every container and tile is placed on whole points: at a bend by every
  look's `Merged::bend`, and by Slide's pose and compose
  (`tiles_sit_on_whole_points_like_the_real_windows`). Layout offsets are fractional (4592.67) and
  the window server places windows on whole points, so a tile drawn at the fraction was resampled
  and popped by the fraction at the lift. Rounding a frame rounds its edges, so its width moves by
  up to a point while no edge moves more than a half. A member a merge moves to another container
  starts at its plan frame and keeps the rounding its tile sat at; placed from the plan frame alone
  it jumped by up to a point, origin and width. A container the merge opens is placed where it
  ends, and `Merged::bend` hands the look its movement from where it installs, where its first
  member's container is drawn. Slide starts that movement at the whole point nearest there, so a
  member of another container moved into it jumps by up to a point, a half for the container and
  a half for its own frame: past a half point on 6 of seeds 1-40 and 163 of the bend contract's
  generator, the farthest 0.82pt on seed 2 (measured with a probe not committed; not seen on
  screen). The test-only Linear look starts it where it installs and stays within a half point
  there. The contract runs seeds 163 and 2 among its `BEND_SEEDS` and allows such a window a
  point (`OPENED_SLACK`, `every_look_bends_a_flight_without_a_jump`). Starting Slide's there too
  keeps every window within a half point, moves the start of such a container's spring by under a
  half point per axis and leaves tile frames, and so where every window lands, as they are, within
  0.88pt of its plan over those 41 seeds, but changes Slide's output (`SLIDE_SCORES`), so Slide
  keeps the whole point.

Found on the first live run of the per-tile strip path: **floating tiles
were drawn behind the desktop backdrop.** The band behind the strip sits
strides deep (1<<20 each) and the backdrop sat at -10000, so the
floating Settings window's tile was in every composition and visible in
none. `BACKDROP_Z` is derived from `z_group::MAX_TILE_DEPTH`, pinned by
`every_possible_tile_draws_between_the_backdrop_and_the_bar`; the containers
sit between it and the bar. During strip pans a floating window deliberately
stands still (`pinned`); during switches it rides its workspace row in the
floating container.

**Edge bounce.** When a push against an end bounces, and what the flight does about it, is "Edge
bounce" in `src/transitions/docs/coordination.md`. A
push is one additive track per container that carries it, its `Bounce::track` (`from, overshoot, 0`
at `0, turn, 1`; the motion curve out, `EASE_IN_OUT` home, the control points of
`kCAMediaTimingFunctionEaseInEaseOut`), which the overlay installs as a `CAKeyframeAnimation` under
its own key, so an in-flight movement is neither replaced nor disturbed and
the model positions stay put; the score's `settled`, every track ended, reads
false until it is home, which holds the lift. The floating container rides only a vertical bounce
(`carries_out_and_back`), the rule a pan (pinned) and a switch (carried) already
follow. Real windows never move. The shape is `Bounce::from_rest`
(`src/animation/domain/motion/bounce.rs`), pinned by
`a_first_push_goes_out_once_and_comes_home`. The overlay installs its track key
for key and curve for curve (`the_bounce_installed_is_its_track`), so
`a_bounce_from_rest_is_the_keyframes_the_overlay_installs` pins what is drawn.

Every push re-kicks the bounce. A push while one plays is `Bounce::from_drawn`
from where it is drawn, the way out shortened by how much of it is already covered
(never below `SHORTEST_PUSH` of a full one) and the way home a full one, added
under the same key so it replaces the one playing
(`a_push_starts_where_the_last_is_drawn`). A burst holds the surface at the wall
and lets it go home once, within one bounce of the last press
(`a_burst_holds_the_wall_and_comes_home_after_the_last_press`). It replaced two
behaviours, both reported in `specs/animation.md`: each press restarting the
keyframes from rest, which bounced once per press and went on bouncing after the
pressing stopped (2026-09-29), and the throttle that fixed it by dropping every
push while one played, which showed nothing for the second and third press of a
burst (2026-10-03). Each container that carries a push starts it from where
its own bounce is drawn, read from the flight's score (`push_tracks` in
`src/animation/domain/motion/accents.rs`): one at rest from rest exactly, and one
still drawing an older bounce than the last, the floating windows under up,
sideways, up within one bounce, from that older one. One bounce pushed from the
engine's record of the last used to be handed to every container, so the floating
container jumped from the older bounce to rest on the last one's clock. A container a merge opens
while a bounce plays draws none of it, opened with no tracks, and the next push starts its bounce from rest on a full
way out while the strip's goes on from where it is drawn on a shortened one: the two stay apart by
at most the offset the strip drew, until both are home. Handed the record's bounce, it used to jump
by up to 72pt into phase with the strip. Both are pinned by
`a_push_starts_each_carrier_from_where_its_bounce_is_drawn`. Every look's push keeps both, half the
pushes against the top or bottom bringing a switch at an end of the stack: no window jumps as a
push reaches it, a push against each of the four walls draws some window toward it, and
every window comes home where its plan sends it, no later than one bounce after the last push or the
motion's own end, which Slide meets exactly over the contract's seed
(`every_look_pushes_out_and_home_without_a_jump`). A
push of no length is a cut, drawn at rest (`a_push_of_no_length_is_a_cut`). A
move's nudge (below) is the other out-and-back, on the same containers by the
same rule, under its own key. Only a step still playing holds a step off (`steps` in
`src/animation/domain/motion/accents.rs` says no while the score `plays` one) and the bounce never
does; pinned by `a_step_is_taken_only_while_none_plays` and
`the_nudge_and_the_bounce_ride_under_keys_of_their_own`.

## The move flight

Which moves step, which way, how the pair is staged and where the real windows go is "The move
flight, staged" in `src/transitions/docs/coordination.md`. This is how it moves.

**The whole strip steps.** It is the edge bounce's mechanism: one additive
keyframe animation, under its own key (`"rini.group.nudge"`), on every
container `carries_out_and_back` names (`step_tracks`), sampled every
1/120s from `nudge_displacement`,
`sin²(πt)`: at rest and motionless at both ends, furthest out at half time.
That is every strip container: the moved window's and the strip's. The
floating container rides only a vertical out-and-back and a nudge is sideways,
so floating windows stay where they are. The first design stepped the moved window's
container alone. The side it opened showed whatever was under the window, and
N crossed on `MOTION_CURVE` in about 60ms, so for nearly the whole step that
was the desktop (a screen recording and the engine's debug log, 2026-09-30).

**The curve.** While it steps, a flight's containers fly on CSS ease-in-out
(`EASE_IN_OUT`, `(0.42, 0, 0.58, 1)`; `flight_curve`) over the step's stretched
duration, or the flight's own when that is longer, instead of `MOTION_CURVE`, so N lingers in the side the step opens.
`MOTION_CURVE` is 87% of the way at a quarter of the flight: it carries N
beneath W before the step is out, and C fills the side instead. `Leg::Curve`
carries its curve and the overlay hands Core Animation the same four numbers
(`timing_function`), so the leg is what the render server draws, and a press
arriving mid-flight bends it from where it is and as fast as it goes; pinned by
`a_leg_on_the_ease_in_out_bends_without_a_kick`. A step that starts on a flight
already moving (a merge) bends the strip's leg as any merge does, a spring, not
the ease-in-out, so N lingers less.

The side the step opens, [V - d, V] with d the step at t, for the design case:
the 1728pt display, full-width W (1722pt at x=3) moved right past N (860pt),
C (1148pt) after N, the view travelling -862, the step 576pt over 0.455s.
Points of it covered, pinned by `the_side_the_step_opens_is_the_simulated_one`:

```
t        0.1  0.2  0.3  0.4  0.5  0.6  0.7  0.8  0.9
opened    55  199  377  521  576  521  377  199   55
N         55  199  377  521  428  283  159   67   14
C          0    0    0    0  145  235  215  129   38
desktop    0    0    0    0    3    3    3    3    3
```

The 3pt of desktop is the gap between N and C. N's tile runs 1727 to 865 on
screen and ends beneath W at [3, 1725]; a move left is its mirror image
(`the_column_passed_fills_the_side_the_step_opens`). On `MOTION_CURVE` N
covers 103 of the 377 points at t = 0.3, and with the step on W's container
alone the opened side shows the desktop
(`the_side_the_step_opens_needs_the_whole_strip_and_the_ease_in_out`). In the
simulation the design was chosen from, flipping the direction alone, W
stepping alone and N crossing, left the whole opened side desktop at every t.
Past the strip's end the opened side shows the desktop: nothing is there.

The nudge does not reuse the bounce's shape: that leaves at 6.25x its average speed, which
over a third of the viewport is the jerk already reported.

```
offset                   nudge, sin², 0.455s    bounce's shape, 0.35s
                         peak                   at launch
573pt  (1720 / 3)        3,956pt/s              29,235pt/s
576pt  (1728 / 3)        3,977pt/s              29,388pt/s
1003pt (3008 / 3)        6,925pt/s              51,173pt/s
72pt, the edge bounce    -                       3,673pt/s
```

The nudge's peak is `offset x π / seconds`, at a quarter and three quarters of
the flight; pinned by `the_nudge_peaks_at_a_moderate_speed`. The viewport is
the display's usable width, `screen.frame`, not the tiling area.

**A burst.** Each press is its own `AnimateSurface`. The first composes the
flight and starts the step. A press arriving mid-flight merges into it
(`merge_group`, Slide's `retarget` over `merge_plans` with the pan): every container moves by the pan,
the new pair votes as an ordinary pass would, and the moved window keeps its
container (a camera that follows it again leaves it going where it was, so it
is not retargeted at all). For a narrower column the newly passed column leaves
the strip's container at the frame it is drawn at and flies to its slot
beneath the moved window, and the pair of the earlier press pans on with the
rest. Containers bend from where the score has their movement at their legs'
speed, never from where they are drawn, and the passed column's new container
carries on the strip's leg (`Leg::toward`), so nothing kicks. For a full-width column each press's passed column is held on
the strip, so nothing leaves the strip's container and the step playing carries
on over all of it (`a_second_full_width_move_mid_flight_holds_its_column_on_the_strip`).
A column an earlier press held pans on with the strip from beneath the moved
window, not to its slot: where the columns passed differ in width it ends a
few points into the outer gap beside the moved window until the lift. A
container a merge opens after a step began does not carry the rest of it: its
member leaves from where it is drawn, so it does not jump, but it stops
stepping, as with the edge bounce. The coordinator's clock restarts to cover
the score. A step still playing is not restarted (`steps`, `Score::plays`): one step
per burst. A press after it has come home, 455ms at the default duration,
starts another. A press at the end of the strip
bounces as before, on top of whatever is playing.

## Resizes through the overlay

A resize rides the per-window overlay path, on the per-tile Core Animation
machinery. The tile travels between its two rects like any other tile; what
changes is how the picture maps onto it (`content_mode` in
`crate::animation::domain::motion::tile`):

- **A movement with a matching picture stretches.** Picture and frame are the
  same size at both ends, so `kCAGravityResize` is exact. Strip movements
  stretch wherever their picture matches, since their tiles never change size
  mid-flight. So does the stand-in, one flat pixel exact at any size.
- **Everything else crops, standing still against the edge that moves less.**
  A resize, a picture lagging one and a grow whose reveal has not landed alike.
  A 3x3 grid of sublayers (`crop`): four corner pieces a band square,
  four edge bands and the middle, every piece mapped 1:1 via `contentsRect`.
  Per axis and per leg the content stands still against the edge that moves
  less from the leg's first key to its last (`Anchors::of_leg`, the top or left
  on a tie), the band on the other edge rides the moving edge, and the seam
  that reveals or swallows content sits one band in from it. A top row growing
  down or shrinking up stands against its top, the reveal at the bottom; a
  bottom row growing up or shrinking down against its bottom, its title band
  riding the top edge; a middle row against whichever edge moves less; a
  window widening leftwards against its right. A new leg picks its anchors
  again, a snap keeps them
  (`a_new_leg_anchors_its_crop_again_and_a_snap_keeps_its_anchors`). The one
  anchor before it, the title band pinned and the content bottom-anchored
  whichever edge moved (ad0ee501, 2026-08-29, chosen because cutting a bottom
  row at its bottom read as the window sliding into a slot), was right only
  where the top edge moves: where the bottom edge moved, the content unrolled
  from under the title band and its top rows were sliced out (recorded
  2026-10-09 on a top row folding a window under it and on a top row
  maximizing; Kai's report in `specs/animation.md` "Resizing"). Content never
  stretches: past the picture's side the middle shows all of the picture, and
  the gap between it and the moving edge's band is the window's ground, a fill
  of its own background colour with its alpha (`Picture::ground`, read off its
  body samples as the tile is first filled, `src/pictures/domain/ground.rs`, in
  the picture's own colour space and pixel format and filled in that space: "A
  picture's ground is read only when a crop-drawn tile asks for it" in
  `src/pictures/docs/README.md`), in
  layers under the picture layer so the corner clip covers them
  (`a_crop_drawn_tile_is_grounded_where_its_picture_ends`). The ground
  covers only what the picture does not, a column and the row either side of
  it meeting without overlap, since a fill under the whole tile would darken a
  translucent window. A grow is held for its repainted picture and flies it
  from frame zero, or, merged once the flight moves, from where its tile is
  drawn as that picture lands; one whose hold ran out flies its before
  picture with the ground in the growth until the repainted one lands
  mid-run, cut onto the grid on the leg's anchors. A leg growing on one axis
  as it shrinks on the other, a fold or an unfold, is two steps on three keys
  (`tile::leg`): the shrinking axis to the corner flying the before picture,
  then the growing axis flying the repainted one, each covering every frame
  of its step, on one curve cut at the corner (`CubicBezier::split`). The
  grid is laid against the before picture at the keys up to the corner and
  against the repainted one from it, the corner keyed twice at one time so
  the grid jumps there, and a discrete keyframe cuts each piece's `contents`
  and the ground's `backgroundColor` at that key time; the model is the
  repainted picture's grid, which the leg ends on
  (`a_leg_in_two_steps_cuts_its_crop_grid_to_its_repaint_at_the_corner`),
  whatever the repainted picture fits, so the before picture always flies up
  to the corner
  (`a_leg_in_two_steps_cuts_to_its_repaint_at_the_corner_whatever_the_repaint_fits`);
  when its ground is read is in "A picture's ground is read only when a
  crop-drawn tile asks for it" in `src/pictures/docs/README.md`.
  Flown in one movement, the repainted picture left the ground in the span
  it falls short on the shrinking axis, and the before picture in the
  growth. A shrink whose tile starts with a picture covering its start,
  frame zero's or the cache's fitting it, flies that picture to the lift,
  clipped, with no ground and nothing swapped onto it ("A grow waits for its
  repaint" in `src/transitions/docs/coordination.md`); an entrance flies its spawn picture
  until its slot picture lands. The stretch it replaced, the middle and the
  whole tile stretched past the picture, read as the window squashing, Kai's
  report. All four rounded corners ride through intact,
  under a hairline drawn at every size ("The hairline and the shadow, as drawn" in
  `src/decorations/docs/README.md`). A first cut used `contentsCenter` (nine-part
  stretching); it kept the corners but read as the window stretching, which
  it is. Wrong-shaped cached pictures use the same mapping instead of being
  dropped — rapid preset cycling used to drop the resized window's tile
  because its picture lagged one press behind. A picture that is a region of
  a batched composite (`src/pictures/domain/region.rs`) is drawn the
  same way: each piece's contents are composed with the tile's region
  (`within`), at rest and at both ends of a resize, and the pieces stay inside
  it however the frame grows, so no piece shows the composite's next window
  (`a_region_pictures_crop_pieces_show_only_the_region`,
  `the_crop_grid_of_a_region_stays_in_the_region`). A tile's region is its
  picture's (`Pixels::unit_rect`), all of the image for anything but a region
  (`a_crop_drawn_tile_takes_its_region_from_its_picture`).
- **The animation is Core Animation end to end.** A grid 1:1 and seamless at
  both endpoint keys stays so at every frame interpolated between them, and
  with the picture at one end's size, as a resize flies, piece frames, contents
  and ground spans are linear functions of the tile frame while the band is
  constant (any side ≥ 89pt), so interpolating between the two endpoint grids
  IS the per-frame crop layout; a leg in two steps holds this on each step,
  between the corner key and the endpoint one; a picture of neither end's
  size, a merged leg starting mid-resize, stays 1:1 but shows its ground
  earlier than the frame's own grid would; one transaction installs frame, contentsRect,
  shadow-path, shadow-mask, hairline-corner, picture-corner and outline-path
  animations on the shared curve, each path and corner at the tile's one corner.
  Below 89pt the band's `min` curve is approximated linearly, drifting the
  seam a few points mid-flight inside the window's own content. A loose
  tile's retarget continues from the position and size the flight's score
  draws it at (`Score::sample`), so rapid preset cycling bends the resize
  instead of snapping it. A retarget that no longer resizes takes away what the
  resize installed: left, its bounds went on to the old size under the new
  move and the tile snapped to its model when the old animation ended.
- **The band is dynamic:** `min(40pt, 45% of the frame's short side)`, and no
  more than half the picture's side, so it degrades continuously into a plain
  reveal as a frame approaches zero — no seam pops mid-flight, no two bands
  ever wider than their frame or their picture.
- The caster's mask is created once per tile and reshaped in place; a
  resize animates its path and the shadow's silhouette between the two
  endpoint shapes, which interpolate because both are built by the same
  constructors.

## The score: what Core Animation plays

`src/animation/domain/motion/score.rs` is the movement of a transition as data: tracks of keyframes
on the strip (a `Zoom`, a scale and a shift), containers (a position), tiles (a frame) and lifts (a
number, how far a tile is lifted), on three channels (`Base`, the movement; `Bounce` and `Nudge`,
added on top of a container's), and the model value each node ends at, which container each tile
rides and how deep everything is drawn. A score is the fold of its edits (`Edit`: a place, a track
added or cut, a ride, a drop, a bank; `a_score_is_the_fold_of_its_edits`). `Score::sample` is what
Core Animation draws at a media time, so a merge bends a flight from the score instead of reading
the presentation layer back ("Who talks to whom" in `docs/pictures-motion-transitions.md`). A look
composes the score and bends it (`src/animation/domain/choreography.rs`; Slide, the default, in
`src/animation/domain/choreography/slide.rs`), the flight keeps it
(`src/transitions/domain/flight_score.rs`), and the overlay plays the same edits, folding each into
a replica it reads only to install: an accent begun again, a crop grid re-keyed
(`the_overlay_plays_exactly_its_score`). The overlay installs every animation from a track, through
one builder (`animation` in `overlay.rs`): a track's `Form`, the constructor that made it, names the
class (`every_track_is_installed_as_itself`), and a tile's track rides every layer of the tile, each
value mapped from its key's frame (`a_tile_track_is_installed_on_every_layer_of_the_tile`); a tile's
new leg judges its content mode again against its first and last key, set only when it changes, and
the leg it is on with its end moved, a snap, keeps it drawn as it was, so a snap never turns a tile
(`a_snap_keeps_how_a_tile_is_drawn_and_a_new_leg_judges_it_again`); a swap judges it again against
the picture landing (`TileOverlay::swap` in "A grow waits for its repaint",
`src/transitions/docs/coordination.md`). The strip's zoom plays on the strip layer's `transform`
under its own key (`rini.strip.zoom`), a matrix of a scale and a translation alone
(`zoom_transform`), so interpolated entry by entry or decomposed it is the score's part-by-part
lerp. That is inferred, not measured on the render server; the read-back pins what is installed
(`the_strip_layer_lets_go_of_its_zoom`). `transform` is the key path the parked zoom spike played
and read back on its strip layer; `transform.scale` carries no camera, and `sublayerTransform` would
draw the same through a layer of its own. The presentation layer is read only by the trace.

Measured 2026-10-04 with `tools/focuslab/playback.swift`: a layer at speed 0 in
an off-screen window, its presentation read at chosen `timeOffset`s.

| What | Core Animation | The score |
|---|---|---|
| Outside `[begin, begin + duration)` | draws the model | `Track::at` is none; `sample` is the model |
| `kCAMediaTimingFunctionEaseInEaseOut` | control points `(0.42, 0, 0.58, 1)` | `EASE_IN_OUT` |
| 1722pt over 0.35s on `MOTION_CURVE` | within 0.088pt of the exact Bezier | `CubicBezier::ease` |
| The 72pt bounce over 0.35s | within 0.072pt of `Bounce::at` | `Bounce::track` |
| Keyframes with no `keyTimes` or `timingFunctions` | evenly spaced, straight lines between | `Track::samples` |
| `duration` 0 | plays for 0.25s | a cut |
| `addAnimation:forKey:` on a key in use | the old one goes | `Score::add` replaces |
| A layer's animations | composed in order of `beginTime`, ties in the order added | `Score::tracks` |
| A non-additive animation beginning after an additive one, or with it and added later | the additive one is not drawn while it plays | `sample` adds every accent playing |
| That accent begun again with it: a copy at its `beginTime`, `timeOffset` as far as the accent had got, `repeatDuration` what is left | drawn exactly where it was, to its old end | `sample` adds every accent playing |

A Bezier segment is the one place the score and the screen part, by Core
Animation's own solver: under 0.1pt. A spring leg is the polyline of its 1/120s
samples, not `Leg::position_at`: from rest the first line cuts the corner by
10.8pt at 1722pt and 5.4pt at 861pt (`a_spring_plays_as_the_polyline_of_its_samples`).
The nudge's polyline is within 0.5pt of `sin²(πt)` at a third of a 1720pt
viewport (`the_nudge_plays_as_its_samples`).

Two rows are rules for whatever plays a score:

- **Order.** Core Animation composes a layer's animations in order of their
  begin times, ties in the order added, and a non-additive one starts over from
  its own value. A movement replaced mid-flight begins after every accent riding
  it, so re-adding the accents unchanged leaves them hidden: they sort before it
  by begin time whatever order they were added in. The score adds every accent
  to the movement whatever order they came in, so a renderer that replaces a
  `Base` track begins the node's accents again with it, each a copy as far into
  its own time as it had got and lasting what is left of it. The overlay does
  this for every movement it gives a container, from the accents' tracks in its
  replica (`install_container_track`, pinned by
  `a_replaced_movement_begins_its_accents_again_from_the_replica` and
  `core_animation_orders_a_layers_animations_by_begin_time`). Until it did, a
  merge hid a bounce or nudge riding a retargeted container until the new leg
  ended.
- **No length.** A track of no length is a cut in the score and 0.25s in Core
  Animation, so a renderer places the node, installs nothing, and takes away
  what played under the track's key, as the score's track replaces it
  (`a_track_of_no_length_installs_nothing`). Slide makes none: what it would move over no length
  it places. A reserved entrance admitted once the clock has run out is the case that reaches it
  ("The reservation fallback" in `src/transitions/docs/coordination.md`).

`Score::settled` is every track ended, and the lift waits for the flight's
(`FlightScore::settled`). The overlay used to compare the presentation with the model within 0.5pt,
which a `MOTION_CURVE` movement of 1722pt reaches at 87% of its duration; the
lift also waits for the clock, so the two part only where a track outlasts it,
and there the score says yes no earlier than the presentation did.
