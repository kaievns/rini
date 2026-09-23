# Cleanup, round 2

Working file for the eleven items agreed after `9ea1079`. Deleted when they are all closed.
Round 1 is in `docs/cleanup-round-1.md`.

Baseline at `9ea1079`: **57,796 non-test code lines, 1,287 tests, 0 warnings**, CI fmt clean.

| # | item | measure | state |
|---|---|---|---|
| 1 | the `?elem` FIXME in `trace` | formatting an `AXUIElement` is an AX round-trip; obeyed on the hot path, ignored on the error path | **done** |
| 2 | `animation/domain/admission.rs` | 125 lines, 7 pure functions, 0 tests | **done** — 25 tests |
| 3 | `input/domain/key.rs` | 515 lines, 1 test, and a 22-line block written twice | **done** — 35 tests |
| 4 | `main.rs` | 300-line `main`, no test | **done** — 300 -> 276, 2 rules out |
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

## 3. `input/domain/key.rs`

515 lines with one test, and the token rule written out twice: once for the words inside
`normalize_spec`'s loop and once for a trailing word with no separator after it. One
`canonical_spec_token` now, and dropping the trailing call fails with `"Alt + Shift + Down"` where
`"Alt + Shift + ArrowDown"` was expected — exactly the failure the duplication invited.

Named `canonical_spec_token`, not `normalize_token`, because there was ALREADY a `normalize_token`
doing the opposite: it lower-cases and strips, for matching a modifier name. One produces the
canonical spelling, the other folds for comparison, and they had the same name.

35 tests, covering the bitfield, generic expansion, token parsing, printing, modifiers-only bindings,
`is_modifier_key` and spec canonicalisation. Two probes, both caught:

| broken | caught by |
|---|---|
| a named side expanded to both sides | `a_named_side_expands_to_itself_only` — `CtrlLeft + A` would fire on right Ctrl |
| the trailing-token canonicalisation | four assertions, with the literal before/after strings |

Three properties are pinned that the code never stated. Every generic modifier is exactly its own two
sides, and no two families share a bit — a collision would make Ctrl and Alt the same modifier.
Generic modifiers multiply: `Ctrl + Shift + A` is 9 registrations and three generic modifiers is 27,
which is the cost of not naming a side. And `expand_modifier_combination` only expands a LEADING
alias followed by ` + `, so `"hyper"` alone does not expand; that is a real limit rather than an
oversight, and it now says so in a test.

## 4. `main.rs`

Two decisions out to `boot.rs`, where `wants_restore` and `config_or_default` already live, with 7
tests. Two phases named. `main` is 300 lines down to 276, and the rest does not want extracting.

- `config_path` — a named `--config` path is taken as given, **including one that does not exist**.
  `config_or_default` reports a missing file as "use the defaults", so falling back to the real config
  here would hide a typo in `--config` behind the user's actual settings.
- `apply_flag_overrides` — and the asymmetry it encodes, which nothing said: `--no-animate` can only
  turn animation OFF and `--default-disable` can only turn the disabled start ON. Neither can undo the
  config in the other direction, because there is no `--animate` or `--no-default-disable`. The flags
  are for a one-off run that differs from the config, not a second place to configure rini. Making
  `--no-animate` two-directional fails `neither_flag_can_undo_the_config_in_the_other_direction`.
- `prepare_process` and `preflight` are named, and the reasons they do what they do are now written
  down: backtraces default on because the crash report that matters comes from someone who did not
  know to set the variable; the accessory activation policy is why rini has no Dock icon;
  `SLSWindowManagementBridgeSetDelegate(null)` detaches the window server's own management bridge,
  without which macOS tiles the windows rini is moving and the two fight; and "Displays have separate
  Spaces" is a hard requirement rather than a degraded mode, because with it off every display shares
  one space and a per-display strip has nowhere to live.

**The remaining 276 lines do not want splitting.** They are nine actors, their channels, and the
wiring between them. A `spawn_actors` function would take about fifteen parameters and read worse than
the sequence it replaced. Length is the wrong measure for a composition root; what mattered was
getting the two DECISIONS out of it, and those are now tested.
