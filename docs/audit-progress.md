# Audit progress

Working file for the ten findings in `docs/implementation-audit.md`. Deleted when
they are all closed.

| # | Finding | State |
|---|---|---|
| 9 | Three parallel KVO observer pairs | **done** `Observed` enum: 8 fns -> 4 |
| — | Two byte-identical display-space one-liners (1.7) | **done** one had 0 callers |
| — | `Observer::new` / `new_with_notification` (1.5) | **done** shared `create` |
| — | Two SkyLight capture paths (1.4) | **done** shared `capture_list_via_skylight` |
| — | Two broadcast builders (1.6) | **done** shared `broadcast_context` |
| 1 | Twelve space-resolution answers | **done** rules -> `space_resolution.rs`, 10 tests; 1 dead method deleted |
| 5 | Two event taps, one lifecycle | **done** shared `Recovery`/`on_recovery`, 6 tests |
| 10 | `gesture_tap.rs` state machine, 0 tests | **done** `SwipeTrack` in domain, 7 tests |
| 6 | `rini-cli` 824 lines, 2 tests | **done** 2 -> 10 tests, incl. a no-collision sweep |
| 8 | `FIXME mod.rs:501` restored state keeps dead apps | **done** stale: already handled at load, and tested |
| 2 | `LayoutEngine` owns 14 things | todo |
| 3 | `Reactor` holds every store | todo |
| 4 | 5,949-line integration test file | todo |
| 7 | `app_actor.rs` 1,376 lines, 0 tests | todo |
