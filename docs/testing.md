# Testing notes

## A fake at the FFI boundary, never a `#[cfg(test)]` branch inside a rule

Two things look alike and are not.

**A fake at the boundary is right.** `window_server.rs` answers its queries from thread-local
overrides under `cfg(test)`, because the alternative is a test that depends on the windows the
developer happens to have open. The section below records the two tests that flapped that way. The
same goes for a `#[cfg(test)]` field like `Record::temp`: there is no field to read in production, so
the accessor has no choice.

**A `#[cfg(test)]` branch inside a rule is wrong,** and it is not a style question. It makes the
suite a test of a different program, and the divergence is invisible at every call site. Four were
found and removed:

| was | the test build ran |
|---|---|
| `spaces.rs::is_user_space` | `true`, unconditionally |
| `reactor/mod.rs` `space_is_user` | `true`, unconditionally |
| `spaces.rs::resolve_command_space` | a different algorithm, both arguments discarded |
| `spaces.rs::resolve_menu_bar_space` | the first screen's space, not the active one |

Two of those hid live bugs. `is_user_space` meant the "only user spaces count" rule this project
enforces was never the rule any test ran; the other meant a window could be followed onto the login
space, and the guard written to prevent it was dead because a later step did the assignment anyway
without checking. Both are in `docs/implementation-audit.md` under 4.4 and 4.4.1.

The test for it: if the thing behind the `#[cfg]` is a QUERY — something the operating system
answers — a fake is fine, and it belongs at the boundary with the other fakes. If it is a RULE —
something rini decides — inject what the rule reads and let the test name it. `SpaceKinds` and
`LiveDisplays` in `src/displays/platform/spaces.rs` are what that looks like.

## A unit test must not read the live window server

`src/windows/platform/window_server.rs` answers queries from fakes and thread-local
overrides under `cfg(test)`. That is the whole mechanism now: one crate, so `cfg(test)`
reaches every module. It used to need a `test-support` cargo feature in three crates,
because `cfg(test)` never fires across a crate boundary, and the reactor's tests had to
turn it on as a dev-dependency to see the fakes at all. Four of the fakes used to fall
through to the real window server when no override was set, which made reactor tests
depend on the windows the developer happened to have open.

Two tests flapped on that, and which of the two failed changed through the day as
windows opened and closed:

```
topology_change_clears_stale_pending_hide_target_before_next_workspace_layout
wsid_rekey_preserves_floating_membership_and_position
```

They flapped because the reactor treats these answers as authoritative, and a
NEGATIVE answer is not neutral: it retires the window.

- `space_window_list_for_connection` is the authoritative membership of a space.
  `reconcile_authoritative_active_window_snapshot` marks every previously visible
  window that is missing from it hidden, then unassigns it from its workspace. The
  test's synthetic window server id was never in the real list, so the window lost
  its workspace and the hidden-window frame write under test never happened. The
  captured list was the real desktop, ids 16, 17, 18, 52, 81 and so on.
- `app_window_suitability` and `window_ordered_in` feed
  `identify_stale_windows`, which retires an AX-omitted window on an explicit
  negative observation. A synthetic id is absent from the real window server, so
  it answered `Some(false)`, the rekeyed window was treated as destroyed, and
  `WindowRemoved` cleared the floating state that the rekey was about to transfer.
- `window_spaces` reports which spaces hold a window. Test ids are small, and so
  are plenty of real ones, so a collision returns another window's spaces.

An unanswerable query and a negative one mean different things to the reactor, so
the test-build answers are the unanswerable ones: `None`, or an empty list. A test
that needs a specific answer says so:

```rust
set_space_window_list_for_space_override(space.get(), Some(vec![wsid]));
set_window_ordered_in_override(wsid, Some(false));
set_window_spaces_override(wsid, Some(vec![space.get()]));
```

Reset each to `None` at the end of the test. They are thread-locals and the suite
runs single-threaded, so a leftover override leaks into whatever runs next.

### How this was found

The reactor test asserted a frame write that never came, and the requests it did
make were only `[GetVisibleWindows]`. Printing the window's workspace at each step
of `handle_authoritative_space_snapshot` put the loss inside
`finalize_space_change`, and printing the authoritative window list showed the
developer's own desktop in it. `std::backtrace::Backtrace::force_capture()` in the
`WindowRemoved` handler named the second one.

## The test suite runs single-threaded on purpose

Several tests reach macOS frameworks that initialise lazily, and initialising
them from more than one thread at once aborts the whole process:

```
objc[75992]: Cannot form weak reference to instance of class
SLSWindowManagementFallbackBridge. It is possible that this object was
over-released, or is in the process of deallocation.
```

Measured rates, 40 parallel runs each: none at all before the overlay work, and
a few percent after it. Narrowing it was misleading, because skipping a test
changes timing as well as coverage, so two different "confirmed" causes both
turned out to be sampling noise. Single-threaded runs never reproduced it across
88 runs.

`RUST_TEST_THREADS = "1"` in `.cargo/config.toml` settles it. The suite goes
from about 0.4s to 1.25s, which is not a trade worth thinking about. The tests
that build capture fixtures also avoid `IOSurface` entirely for the same reason
and use a CPU bitmap instead.
