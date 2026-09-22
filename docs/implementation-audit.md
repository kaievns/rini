# Implementation audit, 2026-09-22

A comb through the tree after the fork's deletions, the crate restructure, and the
recent fold/maximize work. Every entry carries the measurement or `file:line` it
came from. Ranked within each section by what it costs to leave alone.

Measured at commit `118e35d`: **54,751 non-test code lines**, 1101 tests.

Method: normalised function bodies hashed for exact duplicates (1794 functions,
7 groups), `difflib` similarity over 918 production functions ≥10 lines for near
duplicates, caller counts per public method, and per-file code-to-test ratios.
The duplicate-detection pass over FFI declaration blocks produces false positives
(`crates/rini-mach-sys`, `crates/rini-skylight-sys`, `crates/rini-ipc/src/client.rs`); those are
`extern "C"` bodies the parser mis-frames and are excluded throughout.

## 1. Duplicate and parallel implementations

### 1.1 Twelve answers to "which space is this window on"

`src/app/reactor/space_affinity.rs` exposes twelve space-resolution methods over
the same stores. Caller counts:

```
assigned_space_for_window_id                  44
best_space_for_window_id                      26
best_space_for_window                         12
authoritative_space_for_window_id              8
discovery_space_for_window_id                  3
hidden_assigned_space_for_window_id            3
geometry_space_for_window                      3
best_space_for_window_state                    2
pending_target_space_for_window_server_id      2
current_reported_space_for_window_id           0   <- dead
best_space_for_frame                           —   (frame, not window)
hidden_assigned_space_for_frame                —   (frame, not window)
```

`current_reported_space_for_window_id` (line 112) has no callers. It is
`pub(crate)`, so `dead_code` does not see it.

`best_space_for_window` (line 29, 23 lines) and `geometry_space_for_window`
(line 190, 17 lines) are 0.84 similar.

The names do not say how the answers differ — `best`, `assigned`, `authoritative`,
`discovery`, `reported`, `geometry` are six words for six precedence orders over
the same four sources. Picking the wrong one is the shape of several bugs already
recorded in `docs/workspaces/workspaces-and-displays.md`.

### 1.2 Two event taps, one lifecycle, written twice

`src/input/platform/gesture_tap.rs:202` `run` and
`src/input/platform/input_tap.rs:278` `run` are 0.83 similar over 63 and 79 lines.
Both own: a recovery channel, a `tap::ReEnableGovernor`, a cooldown
`RepeatingTimer`, and the same `select!` arm handling `TapDisabled`, checking the
generation, and branching on `ReEnableDecision`.

The *decision* is already shared (`tap::ReEnableGovernor`, which is what stopped
the input freeze recorded in `docs/permissions-and-the-launch-agent.md`). The loop
that drives it is not.

### 1.3 Three parallel KVO observer pairs

`src/windows/platform/app.rs`, each pair differing only in which key path it
observes:

| | similarity |
|---|---|
| `observe_activation_policy` :89 / `observe_finished_launching` :106 | 0.83 |
| `unobserve_activation_policy` :157 / `unobserve_finished_launching` :172 | 0.88 |
| `ensure_activation_policy_observer` :208 / `ensure_finished_launching_observer` :222 | 0.88 |

Six functions, ~89 lines, for two observed properties.

### 1.4 Two SkyLight capture paths

`src/animation/platform/window_snapshot.rs:73` `capture_via_skylight` (31 lines)
and `:204` `capture_composite_via_skylight` (30 lines), 0.83 similar. The
difference is one window versus a window list.

### 1.5 Two AX observer constructors

`src/windows/platform/ax/observer.rs:49` `new` and `:72` `new_with_notification`,
0.98 similar over 17 lines. `new` is `new_with_notification` with one argument
fixed.

### 1.6 Two broadcast builders

`src/app/reactor/mod.rs:1923` `broadcast_window_title_changed` (34 lines) and
`:1958` `broadcast_focused_window_changed` (24 lines), 0.91 similar. Same
resolve-window-then-send shape, different payload.

### 1.7 Two identical display-space one-liners

`src/workspaces/engine.rs:1011` `last_space_for_display_uuid` and `:1026`
`space_for_display_uuid` are both exactly
`self.display_affinity.space_for_display(display_uuid)`.

### 1.8 Four `workspace_for_window` entry points

A delegation chain rather than a contradiction, but four names for one lookup:

- `src/workspaces/domain/assignment.rs:66` — the index, the real one
- `src/workspaces/domain/virtual_workspace.rs:533` and `:542` (`_any`)
- `src/workspaces/domain/window_store.rs:66`

Plus wrappers in `engine.rs`. The `_any` variant exists because the plain one
consults only each space's ACTIVE workspace — a distinction that has already cost
two bugs (`docs/workspaces/launch-memory.md`, "Recording").

## 2. Legacy branching that could be consolidated

Little survives here, because the fork's deletions were thorough and the recent
sweep (`2462e4a`) took the layout-mode vocabulary. What is left:

### 2.1 `FIXME`s that describe live defects, not tidying

```
src/app/reactor/mod.rs:171    receive this event for a space we just switched off of.. FIXME
src/app/reactor/mod.rs:242    FIXME: This can be interleaved incorrectly with the MouseState in app
src/app/reactor/mod.rs:501    FIXME: Remove apps that are no longer running from restored state
src/windows/platform/app_actor.rs:1566  ?elem here can change system behavior
```

`mod.rs:501` is the one with a data consequence: restored state keeps apps that
are gone.

### 2.2 `TODO`s naming known-wrong heuristics

```
src/windows/platform/window_server.rs:441  cgwindowlistcopywindowinfo does not order windows properly
src/windows/platform/app_actor.rs:1221     improve this heuristic using ideas from AeroSpace
src/app/reactor/events/window_discovery.rs:176  Rewrite it
src/app/reactor/events/space.rs:322        we should really know about this app
src/app/reactor/mod.rs:5000                Optimize this with a cache or something
```

### 2.3 What is NOT a finding

Comments containing "used to" are mostly the project's deliberate record of why a
rule exists (`scrolling.rs:3443`, `constraints.rs:16`, `mod.rs:2774`). They were
checked individually; they describe current behaviour and its history, not dead
branches.

Test scaffolding sits behind `#[cfg(test)]` in every case found
(`reactor/testing.rs`, `replay.rs:41`, `spaces.rs:173`, `workspaces.rs:279`,
`focus.rs:113`, `mod.rs:3673`). Nothing ships in the release binary.

## 3. Logic ownership that no longer makes sense

### 3.1 `LayoutEngine` owns fourteen unrelated things

`src/workspaces/engine.rs`, 2,876 code lines, 20 tests (144 lines per test). Its
fields:

```
workspace_layouts  floating  floating_positions  app_rules  focused_window
window_layout_constraints  virtual_workspace_manager  layout_settings
broadcast_tx  display_affinity  launch_memory  connected_displays
persistence  startup_restore_pending
```

A type called `LayoutEngine` owns the IPC broadcast channel, the display-affinity
registry, the launch memory, and the persistence journal. `broadcast_tx` is
already named in `docs/architecture.md` as belonging to `app/api/`.

Its largest methods: `handle_command` (261), `calculate_layout_with_virtual_workspaces`
(203), `move_window_to_workspace` (175), `on_windows_on_screen_updated` (168).

### 3.2 `Reactor` holds every store and 41 event variants

`src/app/reactor/mod.rs`, 4,201 code lines, no in-file tests. Direct field
reaches: `state` 112, `layout_manager` 61, `space_state` 43, `drag_manager` 18,
`communication_manager` 17, `refresh_quarantine_manager` 14,
`transaction_manager` 11, `pending_space_change_manager` 3. Eight "manager"
structs plus `RiniState`.

Largest methods: `handle_layout_response` (297), `dispatch_workflow` (280),
`apply_event_outcome` (269), `handle_authoritative_space_snapshot` (250).

### 3.3 Static methods over `&mut Reactor`

`src/app/reactor/animation.rs` and `managers.rs` take `reactor: &mut Reactor`
rather than `&mut self` on a narrower borrow — 9 signatures. `animate_layout`'s own
doc (`docs/animation/animation-smoothness.md`, "Structural findings") already calls
this out and names the fix: a `present(motion)` boundary.

### 3.4 `src/displays/screen.rs` straddles domain and platform

537 code lines, 2 tests. Already the sole named exception in
`tests/architecture.rs` and explained in `docs/architecture.md`. Repeated here
because it is the only remaining rule-2 violation and the reason the ratchet has
an exception list at all.

### 3.5 `src/workspaces/broadcast.rs` sends IPC from the layout engine

Named in `docs/architecture.md` as belonging to `app/api/`. Unchanged.

## 4. Testability

### 4.1 One 5,949-line integration test file

`src/app/reactor/tests.rs`: 184 tests, 5,949 lines, plus 604 lines of scaffolding
in `reactor/testing.rs`. Every test builds a whole `Reactor`. This is where a
reactor change is verified, which means a reactor change is slow to verify and the
failure points at an orchestration, not a rule.

The recent batches moved the opposite way deliberately — pure decisions to
`domain/` where they are tested in isolation — and that is why
`layout/domain/scrolling.rs` now carries 62 tests at 26 code lines each.

### 4.2 Production modules with no tests at all

| file | code | why it resists |
|---|---|---|
| `src/windows/platform/app_actor.rs` | 1,376 | the AX driver. Needs a fake `AXUIElement` seam |
| `src/displays/platform/spaces.rs` | 1,087 | has `spaces/tests.rs` (42 tests) beside it, so covered |
| `src/input/platform/gesture_tap.rs` | 657 | owns its own state machine inline; the rules left for `domain/gesture.rs` but the loop did not |
| `src/app/reactor/observations.rs` | 574 | gathers from live stores; the shape is right, tests live in `tests.rs` |
| `src/main.rs` | 430 | one 317-line `main`; composition root |
| `src/app/hotkeys.rs` | 404 | one 178-line `handle_event` |

### 4.3 Worst code-to-test ratios among tested files

```
crates/rini-cli/src/main.rs       824 code / 2 tests   412:1
src/input/platform/input_tap.rs   775 code / 1 test    775:1
src/displays/screen.rs            537 code / 2 tests   268:1
src/workspaces/engine.rs        2,876 code / 20 tests  144:1
src/app/reactor/query.rs          806 code / 9 tests    90:1
src/windows/domain/catalogue.rs   796 code / 9 tests    88:1
```

`rini-cli` is argument parsing into wire commands — pure, trivially testable, and
almost untested. Its `parse_direction` plus the command mapping is the whole
surface that can silently mis-map a key to the wrong command.

### 4.4 The 317-line `main`

`src/main.rs:100`. Flags, AX permission, the "separate Spaces" check, config read
with fallback, layout restore, actor construction, joining. The config-fallback
branch (lines 148-168) is a behaviour with a recorded history and no test; it is
reachable only by running the binary.

## Not findings, checked and dismissed

- Exact-duplicate scan over 1794 functions: 7 groups, 5 of them test fixtures, 2
  in `rini-runloop`/`ax/observer` and listed above.
- `ResizeOrientation::Smart`, `MoveNode`, `ContainerTreeNode.weight` and
  `layout_kind`: all carry live values in a scrolling layout.
- `enum_dispatch`: removed in `118e35d`.
- `is_effectively_manageable` appears in `catalogue.rs:70` and `state.rs:30`, but
  the first delegates to the second. One implementation.
