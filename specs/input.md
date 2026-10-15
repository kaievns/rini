# Bindings, modifiers and the event tap

## Key specs

- A binding is written in the config as modifiers and one key joined by `+`: `Alt+Shift+Down`,
  niri's spelling. Spaces MAY surround the `+` in a quoted key: `"Alt + Shift + Down"` is the same key.
- `Mod` MUST stand for the modifier `input › mod-key` names, `Alt` unless written. It is resolved
  whenever the keys are registered, so changing `mod-key` moves every `Mod` bind with it.
- A key MUST name exactly one key. `Ctrl+J+K` MUST be refused: it used to bind Ctrl+K and say nothing.
- niri's triggers that are not keys (`WheelScrollDown`, `MouseLeft`, `TouchpadScrollDown`, `XF86…`)
  MUST be refused with the reason, not as an unknown key.
- A single letter is case-insensitive: `a` and `A` name the same physical key.
- Arrow words are accepted and canonicalised: `Down` means `ArrowDown`, as a whole token between `+`s
  only. niri's XKB names for keys macOS has (`Return`, `BackSpace`, `Page_Up`, `Page_Down`, `space`,
  `grave`) MUST name those keys.
- A token rini does not recognise is left exactly as written. It is either a key name the keyboard
  layer resolves or a typo the config decoder refuses, naming the line, and guessing would turn the
  second into the first.
- Canonicalising a spec twice MUST give the same answer as once.

## Binds

- A key MUST be bound once, across `binds` and `recent-windows › binds` together. A second binding is
  refused with both places named. Keys are compared as pressed: `Mod+H` and `Alt+H` under
  `mod-key "Alt"` are one key, and `LAlt+H` overlaps `Alt+H`. This holds for the config a
  `config set` fragment leaves too, so a fragment changing `mod-key` is checked against the running
  binds.
- `rini.default.kdl` MUST be usable under `mod-key` `"Alt"`, `"Ctrl"` and `"Super"`. Under `"Shift"`
  each `Mod+Shift` bind is its `Mod` bind, and the reference says so.
- A held key MUST fire once. A bind written `repeat=true` fires again on every repeat while held.

> **Reported 2026-10-01.** The switch to KDL asked whether a held key should repeat, niri's default.
> Kai's decision: it fires once unless `repeat=true` says otherwise, because a repeat used to re-fire
> commands such as workspace back-and-forth on every one.

- A bind's workspace and monitor numbers MUST start at 1, the first, as niri's do; `0` is refused.
  `rini-cli` keeps counting from 0, and its help MUST say so wherever it takes a workspace number.

> **Reported 2026-10-01.** Kai's decision on the switch to KDL: `focus-workspace 1` is the first
> workspace, as in niri. Kai's TOML had a comment explaining why `switch_to_workspace = 0` meant the
> first, after binds written from 1 had landed one workspace off.

## Modifier sides

- A modifier named without a side matches EITHER side. `Ctrl+A` fires on left or right Ctrl, or both
  held at once.
- A modifier named WITH a side matches only that side. `CtrlLeft+A` MUST NOT fire on right Ctrl.
- A generic modifier costs three registrations and they multiply: `Ctrl+Shift+A` is nine, and three
  generic modifiers is twenty-seven. Naming a side is what avoids that.
- `l` and `r` prefix a modifier name to select a side. `Option` is `Alt`; `Cmd`, `Command` and `Win`
  are niri's `Super`. `Meta` is not a modifier name.

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
- A tap rini cannot create MUST be logged as an error naming the mask it asked for and its generation,
  and MUST free what was allocated for it. Without the tap no hotkey fires, and a silent failure
  leaves nothing to say why.
- rini MUST NOT tap trackpad gestures. A swipe MUST do what macOS does with it.

> **Reported 2026-10-10.** Of the trackpad swipes rini handled, Kai: "those are all inherited from rift
> and i never used any of them and neither did we review them. some cleaning up would be good". Then:
> "you can delete all the swipe gestures and related code/harness, i don't use them and don't need
> them". The workspace swipe, the strip swipe, their HID tap and the haptic engine went.

## Where it lives

`src/input/domain/key.rs` is the vocabulary, `src/input/domain/binding.rs` what a bind names,
`src/input/domain/hotkey.rs` what a press fires, `src/input/domain/held_keys.rs` which keys are held,
`src/input/domain/switch_session.rs` the switcher's session, `src/input/domain/router.rs` the order
they are asked in for each key event and what is swallowed, `src/input/platform/input_tap.rs` the
tap. Measurements are in
`docs/permissions-and-the-launch-agent.md`.
