# Testing notes

## A unit test must not read the live window server

`crates/rini-macos/src/window_server.rs` answers queries from fakes and thread-local
overrides when built with `cfg(test)` or the `test-support` feature. `rini-wm` turns
the feature on as a dev-dependency, so its tests get the fakes although `rini-macos`
itself is compiled as a normal dependency. `rini-layout` has a `test-support` feature of
the same shape for its test-only accessors (`LayoutEngine::selected_window`,
`WindowStore::debug_assert_invariants`). Four of the fakes used to fall through to
the real window server when no override was set, which made reactor tests depend on
the windows the developer happened to have open.

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
