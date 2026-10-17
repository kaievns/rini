# Roadmap

A working list for one machine and one user. It replaces rift's roadmap rather than editing it,
because much of rift's list is what this fork does not want: more layouts, more settings.

Where a spec or doc tracks an item, it is named. A "Not yet met" line in `specs/` is the
requirement itself; this list only collects them.

## Known bugs

- **A restart loses the strip's scroll offset.** The layout file keeps columns, widths and order,
  not the scroll offset. A column as wide as the viewport comes back at its width but not where it
  stood (`specs/persistence.md`, "Restoring").
- **Some floating drags start no drag session.** A drag session starts only from an Accessibility
  frame report under a held button. It was measured on tiled windows. A title-bar move reaches
  Accessibility for 10-11% of its frames, 46-84ms late at p90. The window server's own stream sees
  all of them (`src/displays/docs/live-frames.md`). With no session, the layout
  reasserts the stored frame.
- **`MouseUp` can interleave wrongly with app-actor `MouseState`.** Layout changes are held while
  the button is down. `MouseUp` arrives on the input thread, frame reports per application, so a
  report made during the drag can land after the release meant to free it. The symptom is a layout
  pass that does not happen, which looks like nothing. It needs a measured case before anyone acts
  on it.
- **The session tap still makes synchronous SkyLight calls.** The hit tests in its callback are what
  stalled when a Screen Recording grant was revoked and froze all input. The re-arm fix removed the
  amplifier, not the trigger (`docs/permissions-and-the-launch-agent.md`).
- **A window that opens or resizes mid-step stops the step.** When a window moves along the strip,
  the whole strip steps aside; a window opening or resizing then leaves the step where it is drawn,
  unfinished (`specs/animation.md`, "Moving a window along the strip").
- **A blur only along a window's edge is missed.** The material test leaves out the outer tenth of
  the window, so a toolbar-only material flies grey (`specs/animation.md`, "Windows with blurred
  materials").
- **Urgency that changes mid-flight turns at the lift.** A tile keeps the urgency its pass gave it
  (`specs/decorations.md`, "Urgent").
- **Which space `CGSGetActiveSpace` answers with two displays needs checking.** The focus
  resolver looks for the key window on that space, probed only with one display. A comment
  inherited from rift said it "does not work", with no case recorded (`specs/focus.md`, "Which
  window is focused").
- **A loose tile's speed jumps when a pass bends it.** Slide starts a resize, an entrance or a
  floating window again on `MOTION_CURVE` from where it is drawn, so its speed jumps at the pass
  (`specs/animation.md`, "Arriving mid-flight").
- **A display with no UUID leaves records that are never pruned.** CoreGraphics reports no UUID
  for it, so it is named `cgdisplay-{id}` from its `ScreenId` (`DisplayUuid::fallback`). The id can
  change across a reconnect, and each new one leaves the old name's records behind in the layout
  file. A reused id hands the old display's homes, strips, widths and launch slots to the next
  display with no UUID (`src/displays/docs/README.md`, "Known debt").
- **A restart once restored a layout mirrored.** After a deploy restart on 2026-10-10 the columns
  came back in reverse order, with one window stacked that had stood alone. Seen once, not
  reproduced; the cause is not known.

Not seen since 2026-08-15, when they were written, and in no spec, doc or test since. Each needs one
check, then a spec or deletion:

- Blank built-in display after manual window moves.
- The first column stays put while the strip shifts on focus, with the external attached.

## Wanted next

- **A flight for the title-bar drag swap.** Dragging one window over another swaps them with no
  motion, which reads as broken (my report, 2026-10-06). A first flight for it went with the
  strip following a live resize, which lagged and was reverted. It has no spec yet.
- **`Mod+R` bound in the defaults.** The specs name `switch-preset-column-width` `Mod+R`, as niri's
  default config binds it, but `rini.default.kdl` leaves it unbound.
- **Skipping a bad bind rather than the file.** Each bind decodes on its own, so every bad bind is
  reported with its line. Today any problem makes the file unusable, as `specs/config.md` requires
  and niri does: at launch rini runs on the built-in defaults, and a reload keeps the running
  config. Skipping a bad bind changes that spec's "When a file cannot be used".
- **A timeout on reactor queries.** A query waits on the reactor with no limit
  (`src/app/reactor/query.rs`). It is the first step of the reactor rework, which I parked on
  2026-10-06.
- **One picture per window.** Each window keeps a focused face beside its picture
  (`specs/animation.md`, "Focus changing during a flight"). Dropping it saves memory. I parked it
  on 2026-10-06, to come back to.
- **The zoom look's open ends.** `look "zoom"` plays a move, a focus press that scrolls, a bounce
  at a side of the strip and a vertical workspace switch on its arc, the strips above and below
  drawn beside it 24pt apart. Open (`specs/animation.md`, "The zoom look"): a press merged into a
  flight restarts its clock at the plain length, past the arc; a layout pass merged into a zoomed
  flight judges which windows it draws against the display at full size; and a switch's floating
  windows travel unzoomed. Kai has not judged the arc on screen since the spike's 3x speed-up, nor
  the strips beside it, nor the switch's arc.
- **Unplug and replug verified end to end,** per arrangement and with role pins, by a
  `query diagnostics` capture before and after into `docs/data/`. Nothing there covers displays yet.
- **Per-display overrides by display name.** `output "UUID"` takes a UUID only, and only `gaps` and
  `struts`; niri names outputs by connector or make, model and serial (`docs/config.md`).
- **Proportional widths in window rules.** `default-column-width` and `default-window-height` take
  `fixed` only (`docs/config.md`).
- **The niri forms refused as not yet supported:** `relative-to="workspace-view"` gradients, a
  ring's `on`, `off` or `width` under `is-active`, unequal corner radii, `geometry-corner-radius`
  under a focus or urgency matcher, `draw-border-with-background true`,
  `center-focused-column "on-overflow"`, negative struts, absolute column widths, `~` in `spawn`,
  and mouse, wheel and tablet triggers (`docs/config.md`). Touchpad triggers are not wanted:
  `specs/input.md` forbids tapping trackpad gestures.
- **Fewer actors on the main thread.** Seven actors that draw nothing share it with the four that
  draw, so a busy one holds a batch the renderer waits on. The cursor warp moves first
  (`docs/architecture.md`, "What is still not where the map says").
- **The other open rows of that table:** the `src/displays/screen.rs` split, typed geometry outside
  the animation features, and the reactor's stores owned by their features, through the
  `present(motion)` boundary its layout pass waits on (`docs/implementation-audit.md`, 3.2 to 3.4).
  The config hub, the controller's lowering and the notifications demultiplexer stay, for the
  reasons their rows give.
- **Grants measured across a reboot.** Not checked since the signing identity came in
  (`docs/permissions-and-the-launch-agent.md`).
- **A faster new terminal window.** Measured 2026-10-09 with a screen recording against the journal:
  0.86s from the key to a usable Ghostty window when Ghostty is in front, 1.47s from another app
  with System Events cold, against an instant foot under niri. Of the warm case, 303ms passes
  before the window exists (the `osascript` launcher and Ghostty), 195ms before rini shows the
  overlay, and 363ms is the opening flight. Three cuts, cheapest first: a smaller Ghostty default
  size, so the window does not open full screen and blank; a rini command that sends an app its own
  new-window key, in place of `osascript` and System Events; a shorter opening flight than the
  0.35s every movement takes.

## Explicit non-goals

The things most likely to look like obvious next steps. The values behind some of them are in the
[manifesto](manifesto.md); the rest cite their spec.

- **More layout modes.** There is one, deliberately.
- **Relying on another app's tooling.** No border tool, no status bar, no companion code for one.
  `rini-cli` is rini's interface for others to call, not a dependency (`specs/README.md`). The one
  exception keeps another app's bar visible through a flight with rini's bar off (`specs/bar.md`).
- **Settings beyond niri's.** Where rini and niri share a concept, rini takes niri's settings and
  names. Where they differ, rini decides. No configuration GUI.
- **Config stability.** Keys change when a better design needs them. The layout file promises only
  that widths stay readable by an older rini (`specs/displays.md`) and that a file of an older or
  newer schema is refused (`specs/persistence.md`).
- **Taking Cmd-Tab.** The private symbolic-hotkey route turns every degraded state into a dead key
  (`specs/switcher.md`).
- **Trackpad gestures.** A swipe does what macOS does with it (`specs/input.md`).
- **Crossing displays along the strip.** Left and right stop at the strip's end
  (`specs/focus.md`).
- **Native Spaces as workspaces.** Impossible without disabling SIP.
- **Upstreaming to rift.** Nearly nothing here is upstreamable.
