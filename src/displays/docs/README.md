# `displays` — screens, native spaces, coordinates

Which physical screens exist, which macOS space each is showing, and how to turn a
point in one coordinate system into another. Depends on `windows`, because "which
windows are on this space" is a window-server query over window ids.

## What it owns

| | |
|---|---|
| `domain/topology.rs` | `ForwardedSpaceState`: one coherent snapshot of screens, spaces and their windows. `analyze_space_snapshot` says what an incoming snapshot changes; `display_set_delta` says which displays arrived, which departed, and whether the set is settled |
| `domain/screen.rs` | `ScreenInfo`, `CoordinateConverter`, `usable_frame` (what the menu bar and Dock leave), `menu_bar_inset`, the visible-space ordering |
| `domain/space_activation.rs` | Which spaces rini manages, and how activation transfers when macOS mints a new space id |
| `platform/spaces.rs` | The spaces actor: the only thing that turns macOS lifecycle signals into a snapshot |
| `platform/space_switch.rs`, `space_query.rs` | Switching to a space, and asking what one is |
| `platform/display_churn.rs`, `cgs_notify.rs`, `window_notify.rs` | Display reconfiguration and the CGS notification streams |
| `platform/cursor_warp.rs` | Moving the pointer when focus crosses displays |
| `platform/mission_control.rs` | Detecting Mission Control, during which nothing may be moved |

## The shape that matters

**A snapshot is never partial.** The spaces actor buffers during sleep, display churn
and lock/login rather than forwarding a half-formed picture, and waits for two
identical topology samples plus a quiet window server before it speaks. Treating an
unstable snapshot as authoritative is what made rini remap every window onto fresh
default workspaces.

**macOS mints a new space id on every reconnect.** One monitor was observed as 479,
484, 487, 516, 552, 1138 in a single session. Nothing durable may be keyed by space
id; display UUID is the stable identity.

**Only user spaces count.** `SLSSpaceGetType == 0`. Fullscreen and login spaces are
transient native state and are nulled out before they can rewrite anything. The two
predicates that decide this are `SpaceKinds`, injected into `AuthorityState` rather than
called directly: they used to be `#[cfg(test)]`-forked functions whose test bodies called
every space a user space, so the invariant was never the rule any test ran.

**Both display sets are read before either is replaced.** A departing display's windows
can only be recorded while the old assignments are still in the store — once macOS has
moved them to the remaining display there is no way to tell which were where. The
reactor used to capture the previous set inline, one line before overwriting it, with a
comment holding the order in place. `display_set_delta` takes both lists as arguments,
so the ordering is in the signature rather than in a comment.

## Reading order

`domain/topology.rs` → `domain/screen.rs` → `platform/spaces.rs`, which is the
hardest file in the tree and worth reading last.

## Detail

- [`topology.md`](topology.md) — why the snapshot is conservative, and the failure that
  taught it

## Known debt

`screen.rs` sits at the feature root rather than in `domain/` or `platform/`, because
it mixes a pure cache with the reads that fill it. It is the sole named exception in
[`tests/architecture.rs`](../../../tests/architecture.rs) and the reason that list
exists; the port it is generic over does not yet cover the space lookup.
