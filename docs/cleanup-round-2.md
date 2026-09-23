# Cleanup, round 2

Working file for the eleven items agreed after `9ea1079`. Deleted when they are all closed.
Round 1 is in `docs/cleanup-round-1.md`.

Baseline at `9ea1079`: **57,796 non-test code lines, 1,287 tests, 0 warnings**, CI fmt clean.

| # | item | measure | state |
|---|---|---|---|
| 1 | the `?elem` FIXME in `trace` | formatting an `AXUIElement` is an AX round-trip; obeyed on the hot path, ignored on the error path | open |
| 2 | `animation/domain/admission.rs` | 125 lines, 7 pure functions, 0 tests | open |
| 3 | `input/domain/key.rs` | 515 lines, 1 test, and a 22-line block written twice | open |
| 4 | `main.rs` | 300-line `main`, no test | open |
| 5 | `layout/domain/scrolling.rs::calculate_layout` | 261 lines, pure, the heart of the tiling | open |
| 6 | `app/reactor/query.rs` | 825 lines, 9 tests (91/test) | open |
| 7 | `windows/domain/catalogue.rs` | 796 lines, 71 public fns, 9 tests | open |
| 8 | `input/platform/input_tap.rs` | 765 lines, 5 tests (153/test) | open |
| 9 | `EventOutcome` | 32 fields, 51 references | open |
| 10 | `engine/persistence/tests.rs` | 3,302 lines, 55 tests, one file | open |
| 11 | `animation/platform/engine.rs` | `start` 294, `begin_group` 253 | open |

## Not on the list, and why

- **`app_actor.rs:887`** — `RaiseCompleted` is sent without checking the raise landed, so it means
  "we asked" rather than "it worked". A correctness gap above everything here on impact, but it needs
  a retry policy decided first: how many attempts, what backoff, and whether a final failure reports
  completion anyway or asks the reactor to re-drive. Its own piece of work.
- **`app_actor.rs:1161`** — replacing the two-bundle allowlist in
  `admissible::needs_title_element_to_be_standard` with a general rule. The TODO names the mechanism
  it needs and `FakeAx` now IS that mechanism, so it is unblocked, but knowing what the general rule
  is needs AX dumps from applications that currently misbehave. Investigation, not refactoring.
