# `displays` — screens, native spaces, coordinates

Which physical screens exist, which macOS space each is showing, and how to turn a
point in one coordinate system into another. Depends on `windows`, because "which
windows are on this space" is a window-server query over window ids.

## What it owns

| | |
|---|---|
| `domain/topology.rs` | `ForwardedSpaceState`: one coherent snapshot of screens, spaces and their windows. `Topology::between` says how far its displays changed against the snapshot before, in tiers that each hold the one before (`Changed`, `Relayout`, `Remap`): `Changed` is the first displays attached, so it is a display set change by construction, and `Relayout` and `Remap` carry whether the `DisplaySet` changed. |
| `domain/topology.rs`, continued | `analyze_space_snapshot` says what an incoming snapshot changes and how far it reaches (`SnapshotReach`: the command space only, something on screen, or far enough that no pending hidden-window target holds); `display_set_delta` says which displays arrived, which departed, and whether the set is settled |
| `domain/screen.rs` | `ScreenInfo` (a display's whole `bounds`, and the `frame` left for windows), `CoordinateConverter` (with `reframe`: where one of rini's own windows has to go, measured against where AppKit has it), `usable_frame` (what the menu bar and Dock leave), `menu_bar_inset`, `top_inset` (the menu bar's inset or a band reserved at the top, whichever is taller), the visible-space ordering |
| `domain/snapshot.rs` | How the spaces actor builds the snapshot it forwards from the screens and the window server's answers, as functions over values: `SpaceKinds`, the classifier it is handed; `user_screens`, which nulls every space rini does not manage and keeps a display's user space while another enters fullscreen |
| `domain/snapshot.rs`, continued | `forward`, which measures a snapshot against what `Forwarded` kept of the last one (its `Topology`, the resized spaces, the space remaps and the display history they keep, the menu-bar and command spaces, the spaces each display owns) |
| `domain/snapshot.rs`, continued | `visible_window_spaces`, which active space each window is on; `window_delta`, which windows a reconfiguration moved |
| `domain/settling.rs` | When the spaces actor asks again: `refresh_retry` under `REFRESH_MAX_RETRIES`, `begins_display_churn`, and `settle`, which counts identical topology samples in `Stability` and says whether a churn's check forwards, checks again, or gives up at `DISPLAY_STABILIZE_MAX_ATTEMPTS` |
| `domain/authority.rs` | What the spaces actor holds between notifications, as three machines, each changed only by its `next` over an edge: `Lifecycle`, the `Hold`s of sleep and the login window and the one release owed to the reactor once a hold comes off; `Churn`, the display reconfiguration under way (`ChurnRun`: its flags, its `Stability`, the windows before it) and the epoch a stale check is told apart by |
| `domain/authority.rs`, continued | `Refresh`, whether a screen refresh is `Pending` and whether a `SpaceSwitch` is unconfirmed |
| `domain/frames.rs` | `latest`: which windows a burst of move and resize notifications names, each once, in the order to read their frames |
| `domain/space_activation.rs` | Which spaces rini manages, and how activation transfers when macOS mints a new space id |
| `platform/spaces.rs` | The spaces actor: the only thing that turns macOS lifecycle signals into a snapshot. It makes the reads, through the `WindowServer` port and `LiveDisplays` (`platform/spaces/live.rs`, with the `SpaceKinds` production hands it), and the domain decides; what it asks again is handed to a `Schedule` (`platform/spaces/schedule.rs`), the main queue in production and a record in tests, so a test runs every retry to its cap |
| `platform/space_query.rs` | Asking what a space is |
| `platform/cgs_notify.rs` | The CGS notification streams |
| `platform/display_churn.rs` | Whether a churn is under way, as the spaces actor alone writes it (`ChurnWriter`) and the reactor reads it (`ChurnReader`) the moment it changes |
| `platform/window_notify.rs` | The actor over those streams: space events to the spaces actor, focus edges coalesced into `WindowServerFocusChanged`, and one `window-frames` thread that reads the window server for the application, in the order it was asked through `Following`'s queue (`ToRead`) |
| `platform/window_notify.rs`, continued | What the `window-frames` thread reads: every tracked window's moves and resizes, as `ServerFrame`s, each reorder, hide, unhide and close, as a `ServerOrder` with the level and sub-level read after a reorder or an unhide (`StackLevel`), and each window the application introduces, whole, as a `ServerIntro`, all handed to its `OnServerRead` callbacks, only while the application is `Following` them ("Live frames" in `live-frames.md`) |
| `platform/cursor_warp.rs` | Carrying the pointer across the edge between two stacked displays, over their whole bounds |
| `platform/mission_control.rs` | Detecting Mission Control, during which nothing may be moved, through the Dock's notifications: a subscription the Dock is too busy to answer is asked again on `src/windows/platform/ax/ask_again.rs`'s schedule for up to 60s |

## The shape that matters

**A snapshot is never partial.** The spaces actor drops every refresh during sleep, display
churn and lock/login rather than forwarding a half-formed picture, resamples after, and waits for
two identical topology samples plus a quiet window server before it speaks. Treating an
unstable snapshot as authoritative is what made rini remap every window onto fresh
default workspaces.

**The application hears a churn twice: as a record and as events.** The flags that start a
churn come in macOS's callback after the reconfiguration (the one before carries only
`BEGIN_CONFIGURATION`, which starts none), so windows can already have moved, and their
Accessibility events, sent from each app's thread, can be on the reactor's queue ahead of anything
the spaces actor sends. So the actor writes the churn to one record shared with the reactor
(`platform/display_churn.rs`) as it begins, before it sends `DisplayChurnBegin`, and the reactor
drops window events while the record says churning, those already queued included. The record is
cleared when the churn settles or is given up on, before a settled one's snapshot is sent, so the
reactor incorporates that snapshot outside the churn. `DisplayChurnBegin` itself, and each
snapshot, go straight to the reactor's queue, as Mission Control and the window notifications send
theirs, so one churn's begin and snapshot cannot pass another's; the reactor holds refreshes from
the begin it hears until the snapshot after it.

**macOS mints a new space id on every reconnect.** One monitor was observed as 479,
484, 487, 516, 552, 1138 in a single session. Nothing durable may be keyed by space
id; display UUID is the stable identity.

**Only user spaces count.** `SLSSpaceGetType == 0`. Fullscreen and login spaces are
transient native state and are nulled out before they can rewrite anything. The two
predicates that decide this are `SpaceKinds`, handed to the spaces actor rather than called
directly: they used to be `#[cfg(test)]`-forked functions whose test bodies called every space a
user space, so the invariant was never the rule any test ran. The classifier tests use,
`SpaceKinds::for_tests`, exists only in a test build, so production cannot construct one with it.

**Both display sets are read before either is replaced.** A departing display's windows
can only be recorded while the old assignments are still in the store — once macOS has
moved them to the remaining display there is no way to tell which were where. The
reactor used to capture the previous set inline, one line before overwriting it, with a
comment holding the order in place. `display_set_delta` takes both lists as arguments,
so the ordering is in the signature rather than in a comment.

## Reading order

`domain/topology.rs` → `domain/screen.rs` → `domain/snapshot.rs` → `domain/settling.rs` →
`domain/authority.rs` → `platform/spaces.rs`, which wires those rules to the lifecycle signals and
is worth reading last.

## Detail

- [`topology.md`](topology.md) — why the snapshot is conservative, and the failure that
  taught it
- [`live-frames.md`](live-frames.md) — the window server's frames, why one thread reads them, and
  how they compare with accessibility's during a drag (measured)

## Known debt

`screen.rs` sits at the feature root rather than in `domain/` or `platform/`, because
it mixes a pure cache with the reads that fill it. It is the sole named exception in
[`tests/architecture.rs`](../../../tests/architecture.rs) and the reason that list
exists. Every read the cache makes goes through the `System` port it is generic over; it still
holds each display's uuid as a `CFRetained<CFString>`, and the free reads beside it have no port.

A display CoreGraphics reports no UUID for is named `DisplayUuid::fallback`, `cgdisplay-{id}`
from its `ScreenId`, in `ScreenCache` (`a_display_with_no_uuid_is_named_by_its_fallback`), and
that name reaches `~/.rini/layout.ron` like a real UUID: the space it owns and the arrangement it
forms are kept under it
(`a_display_with_no_uuid_is_written_to_the_layout_file_under_its_fallback_name`), as is every
other record keyed by display. A `ScreenId` can change across a reconnect, and a display's
remembered space is never pruned, so each new id leaves the records under the old name behind. A
`ScreenId` can also be reused, so the next display with no UUID to get it inherits the old one's
homes, strips, widths and launch slots.
