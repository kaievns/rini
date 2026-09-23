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

Worst ratio of any large production file by 2x. `handle_command` is 262 lines. Most of its
behaviour is reached only through reactor integration tests.

State: open.

## 2. `windows/platform/app_actor.rs` — 1,369 lines, 0 tests

The largest untested production file; needs a fake `AXUIElement` seam. `handle_notification`
166, `handle_request` 157, `handle_raise_request` 134. Carries the three worst TODOs: a
window-matching heuristic known to be wrong (`:1210`), a missing frontmost-window retry
(`:941`), and `FIXME: ?elem here can change system behavior` (`:1554`).

State: open.

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

`retarget` is 109 lines.

State: open.

## 5. Small duplicates

- `ax/permission.rs:28` / `:46` are 0.96 identical; the only difference is `kCFBooleanFalse`
  versus `kCFBooleanTrue`, which is whether the permission dialog appears
- `input/platform/tap.rs` has four near-identical constructors (0.86)
- `windows/platform/app.rs:197` / `:211` — the pair finding 1.3 did not finish

State: open.
