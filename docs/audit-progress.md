# Audit progress

Working file for the ten findings in `docs/implementation-audit.md`. Deleted when
they are all closed.

| # | Finding | State |
|---|---|---|
| 9 | Three parallel KVO observer pairs | **done** `Observed` enum: 8 fns -> 4 |
| — | Two byte-identical display-space one-liners (1.7) | **done** one had 0 callers |
| — | `Observer::new` / `new_with_notification` (1.5) | **done** shared `create` |
| — | Two SkyLight capture paths (1.4) | **done** shared `capture_list_via_skylight` |
| — | Two broadcast builders (1.6) | **gone** with the event subscriptions |
| 1 | Twelve space-resolution answers | **done** rules -> `space_resolution.rs`, 10 tests; 1 dead method deleted |
| 5 | Two event taps, one lifecycle | **done** shared `Recovery`/`on_recovery`, 6 tests; the gesture tap later deleted with the trackpad swipes, so one tap |
| 10 | `gesture_tap.rs` state machine, 0 tests | **done** `SwipeTrack` in domain, 7 tests; both later deleted with the trackpad swipes |
| 6 | `rini-cli` 824 lines, 2 tests | **done** 2 -> 10 tests, incl. a no-collision sweep |
| 8 | `FIXME mod.rs:501` restored state keeps dead apps | **done** stale: already handled at load, and tested |
| 2 | `LayoutEngine` owns 14 things | **done** 14 -> 12 fields: IPC channel became an outbox, since gone with the event subscriptions, and `display_affinity` + `launch_memory` became one `DisplayMemory` on `RiniState`. Schema 5; a refused layout no longer discards the display memory, with 2 tests; files older than schema 6 are now refused, memory read in today's shape |
| 3 | `Reactor` holds every store | **partly** frame writing is now `Present` (`&mut WindowStore` + `&TransactionManager`); `handle_layout_response` 303 -> 248 and `handle_authoritative_space_snapshot` 251 -> 228, 8 rules out to `raise_order`, `boundary` (that rule later deleted with the trackpad swipes, with 5 of the tests), `display_set_delta`, `ArrangeRequest`, `EventOutcome` and `NotificationManager`, 38 tests, and the batching bug that reversed the raise order is fixed |
| 4 | 5,949-line integration test file | **done** 15 fixtures -> `fixtures.rs`, then 184 cases -> 8 subject files under `src/app/reactor/tests/`, largest 1,920 lines |
| 7 | `app_actor.rs` 1,376 lines, 0 tests | **partly** admission rules -> `domain/admissible.rs`, 11 tests; the fake element seam is in (`AxWorld` and `FakeAx` in `ax/world.rs`, `9ea1079`) and the driver has 12 tests on it: frame writes, the sweep, the title element, registration, asking a busy application again, and the thread's end. `handle_notification` and `handle_raise_request` have none |
| 4b | `spaces.rs` quarantine policy inline | **done** the `#[cfg(test)]` forks become an injected `SpaceKinds`; 2 rules out to `domain::topology`; two predicates with one body become `quarantined`; 9 tests |
