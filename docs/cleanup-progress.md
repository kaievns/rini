# Cleanup progress

Working file for the six items agreed after `8274c0b`. Deleted when they are all closed.

Baseline at `8274c0b`: **55,915 non-test code lines, 1,242 tests, 0 warnings**.

## 0. `#[cfg(test)]` branches inside production functions

A `#[cfg(test)]` body inside a function makes the suite a test of a different program. The
three HIGH ones replace a rule outright; the MED ones pin an input to `None`, so every rule
downstream of it is unreachable from a test. See 4.4 in `docs/implementation-audit.md` for
the one that turned out to be a live bug.

| severity | location | what the test ran instead | state |
|---|---|---|---|
| HIGH | `spaces.rs:822` `resolve_command_space` | a different algorithm; both inputs discarded | **done** |
| HIGH | `spaces.rs:861` `resolve_menu_bar_space` | first screen's space, not the active one | **done** |
| HIGH | `mod.rs:869` `space_is_user(..)` | `true`, unconditionally | **done** — and the guard turned out to be dead; see below |
| MED | `spaces.rs:626` `display_space_ids` | derived from screens, not the window server | **done** |
| MED | `spaces.rs:422` `active_display_uuid` | always `None` | **done** |
| MED | `spaces.rs:611` `active_display_uuid` | always `None` | **done** |
| LOW | `spaces.rs:498` `collect_state` | an inline fake screen cache | **done** — the fallback is now always compiled and defensible on its own |
| LOW | `window_server.rs:488,742` | thread-local overrides | **kept** — fakes at the FFI boundary, which is the right pattern; `get_window` moved to item-level `#[cfg]` to match its neighbours |
| LOW | `mod.rs:605` `autosave_path` | `None`, so the suite cannot overwrite `~/.rini/layout.ron` | **done** — set by `new_for_test` instead |
| LOW | `replay.rs:53` `file()` | a temp file | **kept** — `Record::temp` is a `#[cfg(test)]` field, so there is no field to read in production |

### The five in `spaces.rs`

Three window-server reads became `LiveDisplays`, injected beside `SpaceKinds`. Where the test body
and the production body differed structurally, the production body stayed and the test body's
fallback was kept only where it is defensible in production too — the previous snapshot's space when
the current one names none, and the snapshot's spaces when the window server does not answer. One
code path, no branch.

Five tests, three of which fail if the reader goes silent again, which is the state the old bodies
forced.

### What `mod.rs:869` turned out to be

The branch guarded a rule — do not follow a window onto a non-user space — that was NOT being
enforced. With a probe in place the guard ran and correctly declined, and then
`reconcile_windows_with_authoritative_spaces`, one line later in the same function, assigned the
window to that space anyway: it asks `space_resolution::authoritative`, which refuses
`native_fullscreen` but knew nothing about user spaces.

Fixed at the source. `SpaceAffinity::resolve_native_space` now filters its answer through the
classifier, so a login or system space becomes "the window server had nothing to say" and every
rule over `Candidates` falls back to the assignment. The guard was then provably redundant and is
gone. Two tests in `reactor/tests/spaces.rs`, and the positive one — an ordinary inactive space
still gets followed — is what makes the negative one a test of the rule.

**Section 0 closed.** The distinction that came out of it — a fake at the FFI boundary is right, a
`#[cfg(test)]` branch inside a rule is not — is now the opening section of `docs/testing.md`, so the
next person meets it before writing one.

## 1. `workspaces/engine.rs` — 2,898 lines, 20 tests (144 lines/test)

**Partly.** Three rules out, 14 tests. The file's own ratio barely moved, and the honest reason is
that the tests left with the rules: `domain/workspace_focus.rs` holds 11 and
`layout/domain/boundary.rs` gained 3, none of which existed before. Counting lines per in-file test
rewards keeping logic where it cannot be tested, which is the opposite of what this pass is for.

| rule | was | why it is worth naming |
|---|---|---|
| `workspace_focus::preferred` | 56 lines of `if focus_window.is_none()` | six tiers, and the one that matters is invisible in the original: every tiled candidate outranks every floating one, because a floating window sits on top of the strip and focusing one on each switch buries the columns the user switched to see |
| `workspace_focus::cycle_step` | `(idx + len - 1) % len` inline | the indices are unsigned, so stepping back from 0 underflows unless the length is added first |
| `boundary::focus_stays_on_this_display` | two conditions inline | applies to the horizontal axis only; up and down move through the workspace stack, so applying it to all four directions would silently disable vertical navigation between displays |

`handle_command` stays 265 lines for the same reason `dispatch_workflow` does: it is a match on a
command enum where the arms delegate. Its 40-line prelude resolving `(space, workspace_id, layout)`
is the part that is not a dispatch, and it is shared by every arm.

Still open, and named by the RULE rather than by line count this time:

| rule | where | why it is worth naming |
|---|---|---|
| the park corner | `:2385` | a column scrolled off the strip parks in a corner, and which corner records the edge an animation brings it back in from. The wrong one flies the window in from the wrong side |
| the floating rescue | `:2349` | `requested -> stored -> centred`, filtered by "off every screen". What stops a floating window being stranded where no display can show it |
| `op_space` | `:2615` | act on where the window IS, not where the command said. The `if` is a tautology: both branches give the same answer in all three cases, and it collapses to `inferred_space.unwrap_or(space)` |
| `"next"` / `"prev"` | `:2628` | matched as magic NAMES before the name lookup, so a workspace a user actually names "next" cannot be selected by name. Undocumented trap |
| `center_rect` | `:2327` | centre a size in a rect. Trivial, pure, untested |

`on_windows_on_screen_updated` (171) and `move_window_to_space` (145) carry no rules worth
extracting: both sequence store mutations on `was_floating` branches, which is orchestration. The
one candidate is a three-tier target-workspace fallback at `:3091`. They were on the earlier list
because they are long, which was the wrong reason.

## 2. `windows/platform/app_actor.rs` — 1,369 lines, 0 tests

**Partly.** Two rules out to `windows/domain/ax_events.rs` with 12 tests. The file is 1,331 lines and
still has no in-file tests, because the AX seam is not done — see below.

- `handling` / `is_gone` — whether a failed Accessibility request means the window is GONE. Only an
  invalid element does. `AppBusy` (macOS's `CannotComplete`) explicitly does not: an application slow
  to answer is the normal case during launch and under load, and retiring on it deletes live windows.
  `docs/testing.md` records two tests that flapped on exactly this class of mistake. Broadening
  `AppBusy` to retire fails two tests.
- the notification codec — `encode_notification_data` / `decode_notification_data` pack a kind and a
  window index into the single `usize` an observer callback carries. The enum's discriminants ARE the
  wire format, and `from_tag` was a hand-written table that had to agree with them; a variant added
  at the wrong position decoded as its neighbour. `from_tag` now derives from
  `ALL_NOTIFICATION_KINDS` and a round-trip test walks every variant, so a new one fails rather than
  silently decoding as something else. Also pinned: zero is not a kind, because zero is how "no
  window" is spelled in the packed value.

`AxFailure` is the point of the split: the platform side translates macOS's error codes into it in one
place (`State::failure_of`), and the rule reads only that. The architecture test forbids macOS types
outside `platform/`, so a domain rule needs its own vocabulary anyway.

### The AX seam: GENERICS, agreed 2026-09-23

`AXUIElement` is not just called, it is STORED: `AppWindowState.elem` holds one and
`elem_to_wid: HashMap<AXUIElement, WindowId>` keys by one. Faking it means substituting the type
throughout `State`, which is a choice between:

Generics it is: `State<E: Element>`, zero cost at runtime, with the parameter spreading through the
file's signatures and into `spawn_app_thread`. A boxed trait was the alternative and was rejected for
the allocation and vtable hop per AX call on the hot path.

The remaining untested weight behind it: `handle_notification` (166),
`handle_request` (157), `handle_raise_request` (134), and the three worst TODOs in the tree — a
window-matching heuristic known to be wrong (`:1210`), a missing frontmost-window retry (`:941`), and
`FIXME: ?elem here can change system behavior` (`:1554`).

## 3. CI's formatting check is red — 1,126 hunks

`cargo +nightly fmt --all --check` is what `.github/workflows/rust.yml:27` runs. The repo had
never been through it: module declarations unsorted, imports ungrouped.

**Done.** 144 files, 0 hunks remaining under CI's exact command, 1,249 tests still passing and
0 warnings. Nothing but whitespace and import order moved.

Worth knowing for next time: `cargo fmt` prints `can't set brace_style = PreferSameLine, unstable
features are only available in nightly channel` even under `rustup run nightly`. That warning comes
from cargo's own read of `rustfmt.toml`, not from the rustfmt it shells out to — the unstable options
did apply. Check the output, not the warning.

## 4. `animation/platform/overlay.rs` — 1,080 lines, 16 tests (67 lines/test)

**Done, and the ratio was measuring the wrong thing.** The file is a Core Animation driver. Its 16
tests already cover every pure helper in it — `whole`, `tile_shadow_style`, `bar_frame`,
`dressing_piece_indices`, the z-order, `motion_timing` — and what is left untested is
`install_tile`, `ensure_container`, `retarget`, `animate_tile_*` and `apply_edge_dressing`, all of
which manipulate live `CALayer` objects. Those need a compositor, not a better test.

Two pure rules were still in there and are out, with 7 tests, both in
`animation/domain/motion/plan.rs`:

- `Member::start_frame` — a `Rigid` member rides its container, so `rel` is both its start and its
  end; every other variant starts at `from`. It matters when a window is reparented mid-flight: the
  layer installs at the frame the animation is about to run FROM, and reading `to` would land it at
  its destination and animate nowhere.
- `stale_overlay_layers` — which tiles to stop drawing and which containers are then empty. The
  order between the two stages is the content: a container is judged by the tiles that REMAIN.
  Judging against the original set keeps a container whose only tile just left, and an empty
  container is a layer the compositor keeps compositing. Proven by reverting the order — the test
  reports `[]` where it must report `[8]`.

## 5. Small duplicates

**Done**, and only one of the three was duplication.

- `ax/permission.rs` — two bodies differing only in `kCFBooleanFalse` versus `kCFBooleanTrue`,
  which is the difference between asking quietly and interrupting the user with a system dialog.
  One `ax_trusted(Prompt)` now, with the choice named rather than copied.
- `input/platform/tap.rs` — not duplication but a delegation ladder, and four of its six rungs had
  no callers: `new_listen_only`, `new_at_location_listen_only`, and then `new_with_options` and
  `new_at_location_with_options` once the first two were gone. All `pub`, so `dead_code` never saw
  them. Two public constructors remain, both of which the tree actually calls. Two log lines that
  named `new_at_location_with_options` from inside `create` were stale and now say `create`.
- `windows/platform/app.rs:197` / `:211` — **kept.** The 0.88 is a shared two-step lookup; the
  middles differ, and every difference is deliberate. A vanished process still gets the
  activation-policy callback, because rini already has the answer and the caller must hear back; it
  gets no finished-launching callback, because a process that is gone will never finish launching and
  reporting one would announce a launch that did not happen. Neither had a doc comment saying so,
  which was the real gap. Merging them behind a flag would hide exactly those decisions.
