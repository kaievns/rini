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
| 5 | `layout/domain/scrolling.rs::calculate_layout` | 261 lines, pure, the heart of the tiling | **done** — 261 -> 229, 24 tests |
| 6 | `app/reactor/query.rs` | 825 lines, 9 tests (91/test) | **done** — 9 -> 21 tests, 3 rules out |
| 7 | `windows/domain/catalogue.rs` | 796 lines, 71 public fns, 9 tests | **done** — 9 -> 23 tests, and a prune bug |
| 8 | `input/platform/input_tap.rs` | 765 lines, 5 tests (153/test) | **done** — `HeldKeys` out, 12 tests |
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

## 5. `calculate_layout`

The first 75 lines were one coherent thing: how wide each column is. Out to
`layout/domain/constraints.rs` as `column_limits` and `column_width`, with 24 tests. 261 lines down
to 229; the rest assigns frames from the widths and is not separable from the strip it walks.

Three probes, each caught:

| broken | caught by |
|---|---|
| smallest lock instead of largest | `two_locks_in_one_column_take_the_larger` |
| a maximum allowed to beat a minimum | the contradiction test, 300 where 700 was required |
| the viewport clamp | `no_column_is_ever_wider_than_the_viewport` |

**A test corrected my reading of the design.** I asserted that a lock pins a column in both
directions. It does not: `normalized` never derives a maximum from a lock, so a lock raises the
column's FLOOR and leaves it free to be wider. Capping is the maximum's job, and the window itself is
held to its lock separately by `clamp_to_constraints` — which is the better behaviour, because a
non-resizable window in a wide column sits at its own size with space beside it rather than shrinking
the column and dragging its neighbours along. The test now says that, and a second one covers a window
reporting a lock and a maximum together, which does pin the column.

Four rules are now stated that were only implied by the arithmetic: a column's windows agree
pessimistically (largest minimum, smallest maximum, larger lock); a zero maximum means NO maximum,
because macOS reports 0 for an unconstrained window and reading it literally would collapse the column;
a minimum beats a contradicting maximum, because clipped at the edge is recoverable and too small to
use is not; and the gap comes out of the columns rather than from between them, which is what makes two
half-width columns plus their gap add up to exactly the viewport.

## 6. `app/reactor/query.rs`

Three rules out to `app/reactor/diagnostics.rs` with 12 tests, and query.rs's own tests go 9 to 21 —
every handler that had none now has one.

- `default_query_space` — the three-tier precedence a query falls back on. Its cost is already
  recorded on `query_diagnostics`: a `query windows` naming no space answers about ONE space, so
  windows on the other display read as absent, which produced three wrong conclusions in a row.
  Reordering the tiers fails.
- `orphaned_windows` — owned, tiled, and absent from the layout tree: cmd-tab reachable and
  unreachable by scrolling, which is what "a second invisible strip" looks like from the user's side.
  Floating windows are excluded because being outside the tree is what floating MEANS, and counting
  them would make every space with one look broken. Dropping that exclusion fails.
- `stale_display_homes` — a home recorded for a window that no longer exists. Harmless singly, but the
  count growing across a session is evidence that some path removes a window without going through
  `forget_window`.

**A test corrected my reading again.** I asserted that a space rini has never seen lists no
workspaces. It lists all of them: `ordered_workspace_ids` ignores its space argument on purpose —
"index 2 is the same workspace on every display" — so the workspace ORDER is global while the ACTIVE
workspace is per space. The test now pins that distinction, which is more useful than what I first
wrote, and notes that the reactor-level `querying_an_unknown_space_creates_nothing` passes only
because a fresh reactor has no workspaces at all yet.

## 7. `windows/domain/catalogue.rs`

9 tests to 23, and a real defect in the prune predicates.

**`prune_window_record` checked four of `WindowRecord`'s nine fields.** The five it ignored included
`pending_operation`, so a record holding only a pending frame write was pruned and the write
forgotten, and `native_space`, so a record holding only a native-space observation lost it.

Both predicates are now exhaustive destructures with no `..`, which makes the COMPILER the guard
rather than a test: adding a field to `WindowRecord` fails with

```
error[E0027]: pattern does not mention field `a_new_field`
```

until someone decides whether the new field counts as something to remember. A test cannot catch a
field that does not exist yet; a destructure can. Verified by adding one.

The 1,392 existing tests all still pass with the stricter predicate, so nothing depended on the looser
one — it was discarding state nobody had noticed.

**A test corrected my reading a third time.** `set_visible_windows` only ADDS; it does not hide the
windows it omits. That is why `update_complete_window_server_info` clears first and
`update_partial_window_server_info` does not — a complete snapshot means "these and no others", a
partial one means "at least these". The hazard is silent: a caller treating a partial snapshot as
complete leaves a closed window marked visible, and the reactor treats visibility as authoritative.
Both halves are now pinned.

## 8. `input/platform/input_tap.rs`

The pressed-key tracking is out to `input/domain/held_keys.rs` with 12 tests, and the flag helpers it
needed went to `input/domain/key.rs` where the masks already were.

**Two sources that do not agree, which is the whole subject.** A key-down/key-up pair is an EDGE: it
says what changed, and a disabled tap or a dropped event means an edge was missed and the cache is
wrong. The modifier flags on every event are a LEVEL: authoritative about what is held right now, but
only about modifiers. So modifiers are answered from the flags, everything else from the cache, and the
cache is discarded whenever the tap comes back.

Getting it wrong looks like rini ignoring the keyboard: a binding that fires with nothing held, or one
that never fires until the user presses and releases a modifier to resynchronise.

| broken | caught by |
|---|---|
| answering a modifier from the cache | 4 tests, including "no flag bit set, so it is not held whatever the cache says" |
| reconciling instead of clearing on tap re-enable | "including one that may still be down" |

`modifier_key_is_active`, `modifier_mask_for_key` and `ModFamily::{is_active, left_is_active,
right_is_active}` moved from `platform/keyboard.rs` to `domain/key.rs`, taking `u64` rather than
`CGEventFlags`. They were in platform only because of the parameter type — the masks were already in the
domain table, whose own comment says it "needs no CoreGraphics". Platform keeps two one-line wrappers
that call `.bits()`.

Three rules are now stated that the code only implied: the family mask alone does not hold either side
(so a `CtrlLeft` binding does not fire on right Ctrl), reconciling must not drop ordinary keys because
the flags say nothing about them, and the lock keys are held by their flag alone because macOS reports
them as flags and never as edges.
