# Testing notes

## A unit test must not read the live window server

`sys/window_server.rs` answers queries from fakes and thread-local overrides in
test builds. Four of them used to fall through to the real window server when no
override was set, which made reactor tests depend on the windows the developer
happened to have open.

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

See `docs/capture-overlay-research.md`. `IOSurfaceLock` from parallel test threads
races SkyLight's lazy initialisation and aborts the process, so `.cargo/config.toml`
sets `RUST_TEST_THREADS = "1"`.
