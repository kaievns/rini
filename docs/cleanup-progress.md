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

### The AX seam: done, generics

`State<W: AxWorld>`. `AXUIElement` mentions in the actor went from 22 to 7, and the only live one
left is `AXUIElement::application(pid)` in `spawn_app_thread`, which is where the thread is composed.
`MacAx` is the calls the actor used to make inline; `FakeAx`'s elements are plain numbers.

Landed in three commits so the risky one had a green fallback: the trait (`c5c215f`), the
behaviour-preserving conversion (`f9edb7c`), then the fake and the first tests this file has ever had.

Three things moved rather than translated, because they are questions about the application and the
world is what answers those: the enhanced-UI refcount, notification registration (`watch`/`unwatch`,
so the observer is no longer a `State` field), and `isTerminated` (`app_has_quit`, which also removed
`running_app` from `State` and is what made a test `State` constructible at all).

**The test the seam was worth building for.** A frame write is `set_size`, `set_position`,
`set_size` — sized twice. It looks redundant enough that a tidy-up would drop one, and nothing
pinned it, because this file had no tests. AppKit clamps a size against the window's CURRENT
position, so a window near a screen edge cannot grow until it has moved; the first size is what lets
the move succeed for a window that has to grow and shift at once. Dropping either one now fails.

Each of the five was checked by breaking what it guards:

| test | broken by | reports |
|---|---|---|
| frame write order | dropping the first `set_size` | "sized twice, before and after the move" |
| sweep keeps a busy app | `AppBusy => Retire` | "a busy application has not lost its windows" |
| registration subscribes | skipping `watch` | "AXUIElementDestroyed was never subscribed" |
| sweep retires a dead element | — | pins that only an invalid element retires, and exactly one event is sent |
| title element | — | pins `needs_title_element_to_be_standard` end to end |

One trap for whoever writes the next test: `admissible::has_visible_peer` refuses a window that has
a window-server id but no window-server RECORD, because an id the server will not report back means
the window is not on screen. So `register_window` needs a `WindowServerInfo` hint, which is what the
`peer()` helper in the test module is for.

Still untested in there: `handle_notification` (166), `handle_request`'s other arms, and
`handle_raise_request` (134), which is async and drives activation waits. The three worst TODOs are
also still live — the window-matching heuristic (`:1210`), the missing frontmost retry (`:941`), and
`FIXME: ?elem here can change system behavior` (`:1554`). The seam is what makes any of them
testable; none of them is done.
