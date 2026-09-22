# `input` — what the user asked for

Keys, bindings, gestures and drags. It emits `rini-ipc` commands, so the reactor cannot
tell a hotkey from a CLI call — which is why every binding is testable as a command and
why the CLI is a first-class client rather than a debugging aid.

## What it owns

| | |
|---|---|
| `domain/key.rs` | `Modifiers`, `KeyCode`, and the token parsing that needs no keyboard |
| `domain/hotkey.rs` | `modifiers_satisfy`: whether the keys held down are the ones a binding asked for |
| `domain/binding.rs` | `WmCmd`/`WmCommand`: the binding table and what a config string parses to |
| `domain/pointer.rs` | When a mouse move is worth processing, when the cached pointer window still answers, and which events the tap asks for at all |
| `domain/gesture.rs` | Trackpad rules over normalised touches: `swipe_step`, `scroll_step`, `touch_centroid`, and `SwipeTrack`'s phase machine |
| `domain/drag_swap.rs` | Recognising a drag that means "swap these two windows" |
| `platform/tap.rs` | `EventTap`, plus the shared tap lifecycle: `ReEnableGovernor` and `on_recovery` |
| `platform/input_tap.rs` | The session tap: mouse buttons, and keys whenever a hotkey is bound |
| `platform/gesture_tap.rs` | The HID tap for trackpad gestures |
| `platform/keyboard.rs` | The layout-dependent `FromStr` impls — the only part that needs the current keyboard |
| `platform/haptics.rs`, `cursor.rs` | The haptic engine and pointer control |

## The shape that matters

**An active tap sits in the delivery path.** The window server holds each matching
event until the callback answers, so a slow callback freezes all input. That happened:
see "A revoked screen-recording grant froze all input" in
[`docs/permissions-and-the-launch-agent.md`](../../../docs/permissions-and-the-launch-agent.md).

**macOS disables an unresponsive tap, and rini must not fight it.** Re-enabling inside
the callback defeated the safety valve and made the freeze survive until reboot.
`ReEnableGovernor` decides when to re-arm; `on_recovery` decides whether a message is
even about the current tap, because re-arming a replaced generation puts a second tap
in the path swallowing events.

**Rules are separable from taps.** Every gesture decision is a pure function over
normalised positions, so it can be exercised without a trackpad.

## Reading order

`domain/gesture.rs` → `platform/tap.rs` → then either tap.

**The mask is a decision, not a constant.** Mouse buttons are always asked for, because
a drag is how a window moves between displays. Keys are asked for only when something is
bound, and mouse moves only when focus-follows-mouse is both configured and live. The two
halves are independent, so a machine that only uses hotkeys does not pay a window-server
query per pointer movement.

## Known debt

`input_tap.rs` is still the largest file here. Its pure decisions are now
`domain/pointer.rs` and the mask has tests, but the callback, the hotkey table and the
modifier bookkeeping remain, and those need the tap.
