# Cleanup, round 2

Working file for the eleven items agreed after `9ea1079`. Deleted when they are all closed.
Round 1 is in `docs/cleanup-round-1.md`.

Baseline at `9ea1079`: **57,796 non-test code lines, 1,287 tests, 0 warnings**, CI fmt clean.

| # | item | measure | state |
|---|---|---|---|
| 1 | the `?elem` FIXME in `trace` | formatting an `AXUIElement` is an AX round-trip; obeyed on the hot path, ignored on the error path | **done** |
| 2 | `animation/domain/admission.rs` | 125 lines, 7 pure functions, 0 tests | **done** — 25 tests |
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

## 1. The `?elem` FIXME

`AXUIElement`'s `Debug` delegates to the Core Foundation description, which queries the element for
its role and title. Formatting one is a round-trip to the application, and on a wedged application it
blocks — inside a log line, which is the last place anyone looks for a stall.

The FIXME was half-obeyed. `trace`'s hot-path `trace!` had the field commented out; the error arm
three lines below still formatted the element, in the branch reached when the application is hung.
Three more sites did it too, including the failure path of notification registration.

`trace` now takes `about: impl Debug` — a window id, or the pid for an application-level call — and
nine call sites pass what they already had in scope instead of threading an element through.

Guarded by `no_accessibility_element_is_ever_logged` in `tests/architecture.rs`, because a comment
did not hold. Reintroducing it at the `watch` failure site fails with the file, line and the offending
code; reintroducing it in `trace` itself no longer compiles, since there is no element to format.

## 2. `animation/domain/admission.rs`

Seven pure functions governing what happens when a layout pass arrives while windows are still
flying, and no tests at all. Every function is `pub(in crate::animation)`, so `dead_code` could not
see them either.

25 tests. Three of the rules were probed by breaking them, and each was caught:

| broken | caught by |
|---|---|
| `same_as` -> `==` in `merge_action` | a destination differing by floating-point noise reads as a retarget |
| the capture-budget check | a spent budget stops being reported, and reports the picture instead |
| the frame-zero hold branch | a holding flight sends only the newcomers instead of everything |

Two rules turned out to be worth stating rather than just covering. `merge_action` uses `same_as`
rather than `==` so a repeated key press is `Redundant` rather than `Retargeted` — otherwise a held
key bends the tile toward where it already is on every repeat and the flight never lands.
`entrance_plan` checks the capture budget BEFORE the picture, so a flight that has spent its captures
says so rather than blaming a picture it never took; each of the four refusals names itself, because
"the window appeared without animating" has four causes and the log is the only way to tell them
apart afterwards.
