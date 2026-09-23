# Bindings, modifiers and gestures

## Key specs

- A binding is written in the config as modifiers plus a key: `Alt + Shift + Down`.
- A single letter is case-insensitive: `a` and `A` name the same physical key.
- Arrow words are accepted and canonicalised: `Down` means `ArrowDown`.
- A token rini does not recognise is left exactly as written. It is either a key name the keyboard
  layer resolves or a typo the config validator refuses, and guessing would turn the second into the
  first.
- Canonicalising a spec twice MUST give the same answer as once.

## Modifier sides

- A modifier named without a side matches EITHER side. `Ctrl + A` fires on left or right Ctrl, or both
  held at once.
- A modifier named WITH a side matches only that side. `CtrlLeft + A` MUST NOT fire on right Ctrl.
- A generic modifier costs three registrations and they multiply: `Ctrl + Shift + A` is nine, and three
  generic modifiers is twenty-seven. Naming a side is what avoids that.
- `l` and `r` prefix a modifier name to select a side. `option` is `alt`; `cmd` and `command` are `meta`.

## Which keys are held

Two sources that do not agree, and both are needed:

- A key-down/key-up pair is an EDGE. It says what changed, and a disabled tap or a dropped event means
  an edge was missed.
- The modifier flags carried by every event are a LEVEL. They are authoritative about what is held
  right now, but only about modifiers.

So a modifier MUST be answered from the flags and anything else from the edge cache. When the tap is
re-enabled the whole edge cache MUST be discarded rather than reconciled: any number of key-ups may
have happened while it was off.

Getting this wrong looks like rini ignoring the keyboard — a binding that fires with nothing held, or
one that will not fire until the user presses and releases a modifier to resynchronise.

The three lock keys — caps lock, fn, num lock — are reported by macOS as flags and never as edges, so a
binding naming one is satisfied by the flag alone.

## The event tap

- rini MUST NOT ask for keyboard events when nothing is bound. An active tap sits in the delivery path
  and the window server holds each matching event until the callback answers, so asking for keys nobody
  is listening for puts rini between the user and every keystroke for nothing.
- A tap the OS disables MUST be re-armed, and a re-arm MUST be matched to the generation that was
  disabled, or a stale recovery re-enables a tap that has since been replaced.

## Gestures

- A horizontal swipe scrolls the strip. When the strip has run out in that direction, the swipe MAY
  continue into the workspace stack, according to configuration.
- Only a HORIZONTAL edge propagates. A vertical swipe already moves through the workspace stack, so
  propagating a vertical edge would step it twice.
- `invert_horizontal` swaps which workspace a left or right edge steps to. The two directions MUST NOT
  ever answer the same way.

## Where it lives

`src/input/domain/key.rs` is the vocabulary, `src/input/domain/held_keys.rs` which keys are held,
`src/input/domain/gesture.rs` the swipe phases, `src/input/platform/input_tap.rs` and
`gesture_tap.rs` the taps. Measurements are in `docs/permissions-and-the-launch-agent.md`.
