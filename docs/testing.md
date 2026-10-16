# Testing notes

## A fake at the FFI boundary, never a `#[cfg(test)]` branch inside a rule

Two things look alike and are not.

**A fake at the boundary is right.** The window server is a port, `WindowServer`, faked in memory
by `FakeWindowServer`, because the alternative is a test that depends on the windows the developer
happens to have open. The section below records the two tests that flapped that way.

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
something rini decides — take what the rule reads as a value and let the test name it.
`manageable(Facts)` in `src/windows/domain/manageable.rs` is what that looks like, and so is
`KeyRouter` in `src/input/domain/router.rs`: the tap reads each CGEvent into a `Keystroke`, and a
test builds one by hand. Where a thread both decides and makes the calls, the calls are a port it is
generic over: the pictures service is `Service<C: Capturer>` (`src/pictures/platform/capturer.rs`),
and its tests run its rounds against `FakeCapturer`, which answers from the frames a test gives it
and records every call made
(`overlapping_windows_go_in_calls_of_their_own_and_every_set_in_its_own`). A fake gives only answers
the real calls can: its misshapen call is the baked one, so it yields no picture and its windows are
opaque, as in `skylight::capture_on_screen`.

A rule about which AppKit or Core Animation calls are made, in what order and with what geometry, is
a rule too: the domain returns it, as a plan of calls or as the values the calls take, and the
platform applies it, so a test reads it. `rest::plan_draw` in `src/decorations/domain/rest.rs`
returns the calls of a draw in order; `placement::frames` in `src/bar/domain/placement.rs` returns
the frames a bar draw puts, and `BarPanel::draw` still decides the order it puts and hides them in.

## Which seam to use

Reach for the seam where the read is made; a rule below it takes the answers as values and needs
none. Each boundary has one seam but the window server, which has more: `WindowServer`; the spaces
actor's `LiveDisplays` (the active space, the menu-bar display, each display's spaces and the quiet
clock) and `SpaceKinds` (each space's kind), beside the port; the pictures thread's `Capturer`,
which makes its captures as well as its reads; and the flight engine's `Capture`, the one capture it
takes itself, of a window that just opened, beside the `WindowServer` it reads.

| what the code asks | seam | production | a test passes |
|---|---|---|---|
| the window server: windows, frames, levels, spaces, order, the space lists, the key window, the listings and the name each window's owner goes by, a display's desktop windows; `make_key_window`, `take_stack_place`, and `focus_desktop` over them | `WindowServer`, an `Arc<dyn WindowServer>` (`src/windows/platform/window_server/port.rs`) | `window_server::live()` | a clone of an `Arc<FakeWindowServer>`, answers stated with its `set_` methods, commands read back with `commands()`, reads counted with `reads()`; every read unknown until stated |
| what the reactor asks of the machine: the window server, the desktop focus included, the pointer, Low Power Mode, the displays' bounds, display churn, the space kinds, the running applications; and the two notes it hands back, the window-server activity clock and the windows it wants notifications for | `SystemReads`, one value of the `WindowServer` port, the churn's `ChurnReader` and fn pointers (`src/app/system_reads.rs`). A reducer takes the answer it needs as a parameter. The five that judge manageability (`handle_window_created`, `handle_window_deminiaturized`, `sync_existing_window_state`, `process_window_list`, `update_window_states`) take the port instead: their facts are read lazily, each only while those in hand do not turn the window away (`turned_away_before_accessibility`), so no stickiness or level is read for a minimized window or one off layer 0 | `SystemReads::live`, built in `Reactor::spawn` over the reader of the record `main` hands the spaces actor the writer of | `SystemReads::fake`, which `Reactor::new_for_test` builds over a `FakeWindowServer` of its own, stated through `reactor.window_server()`; a field swapped for a fn of the test's, `reactor.reads_mut().low_power = \|\| true`; every read unknown, false or none until then (`a_test_reactor_reads_unknown_from_everything_it_was_not_told`); a churn begun with `reactor.begin_display_churn()` |
| Accessibility, the window server, the pointer state and Command-W, from the per-app thread | `AxWorld`, a type parameter (`src/windows/platform/ax/world.rs`), whose `server()` is the `WindowServer` port; the thread asks nothing past it and its tests take no path through `window_server` or `mouse`, `MouseState` re-exported from the world (`the_app_actor_reaches_the_platform_only_through_its_world`) | `MacAx`, its server `window_server::live()` | `FakeAx`, whose elements are numbers; an element with nothing recorded answers as a window that has gone; its server a `FakeWindowServer` (`made_key()` the windows made key), its pointer state `mouse`, its Command-W posts counted in `command_w_posts` |
| displays and spaces, from the spaces actor | `LiveDisplays` (`src/displays/platform/spaces/live.rs`) and `SpaceKinds` (`src/displays/domain/snapshot.rs`), structs of fn pointers, beside the `WindowServer` port for each space's windows and the spaces each window is on; `window_space` takes the actor's `SpaceKinds` to pick a window's home among them, so whether a space is a user space is stated once, by the classifier, and the port has no read of it | `from_window_server()`, and `window_server::live()` | `LiveDisplays::silent()`, which knows nothing, and `SpaceKinds::for_tests()`, which only a test build has, or a struct of its own; a `FakeWindowServer` (`hand_window_server`) |
| when the spaces actor asks again: a refresh's retries, a churn's checks | `Schedule`, a trait object (`src/displays/platform/spaces/schedule.rs`) | `MainQueue`, the main dispatch queue | `Recorded`, which keeps every notification asked for and sends none, so a test feeds them back to run a retry to its cap (`a_churn_that_never_settles_is_checked_up_to_its_cap_then_ends_with_a_refresh`) |
| the framed capture of a window that just opened, from the flight engine | `Capture`, a boxed fn in `Parts` (`src/transitions/platform/engine.rs`) | `skylight::capture_via_framed` | a fn answering the picture the test gives for each window, recording each one asked |
| the window server and ScreenCaptureKit, from the pictures thread | `Capturer`, a type parameter (`src/pictures/platform/capturer.rs`) | `MacCapturer` | `FakeCapturer`, answering from the frames a test gives it and recording every call |
| the screens, for `ScreenCache`: the displays, their uuids and scales, the main and the built-in display, the menu bar and the Dock, the space each display shows | `System`, a type parameter (`src/displays/screen.rs`) | `Actual` | a `System` of its own, as the stubs in `screen.rs`'s tests are, whose menu bar is hidden, Dock not shown and spaces unknown until stated |

Every caller of the window server holds the port, and `window_server` offers no free function of a
read's or a command's name to call around it (`the_window_server_is_reached_only_through_its_port`).
The reactor reads through its `SystemReads`, the flight engine through its `Parts`
(`the_flight_engine_reads_the_window_server_only_through_its_port`), the app actor through its
`AxWorld`'s `server()` (`the_app_actor_reaches_the_platform_only_through_its_world`), and the spaces
actor, the frames thread and the focus resolver of `WindowNotify`, the rest borders, the input tap,
the bar's extras and grounds and the pictures service's `MacCapturer` through the one their
production constructor takes from `window_server::live()`. A read that runs on another's behalf
takes the caller's port: `Pictures::dressing` and `skylight::capture_via_framed` read through the
flight engine's.

## A unit test must not read the live window server

A test asks the window server through `FakeWindowServer`, handed to the code under test as its
`WindowServer` port. Every read of the fake answers "unknown" until the test states an answer
(`every_read_answers_unknown_with_no_setup`), and a command is recorded, never carried out
(`make_key_window_and_take_stack_place_only_record`), so no test reads the live window server or
makes a real window key. The live port is out of a test's reach: `window_server::live()` panics in a
test build and nothing outside `window_server` builds a `LiveWindowServer`
(`a_test_build_never_takes_the_live_window_server`), so a test that builds a production reactor or
actor through it fails rather than reading SkyLight. Before the port, `window_server.rs` answered
from thread-local overrides under `cfg(test)`, and four of those fakes fell through to the real
window server when no override was set, which made reactor tests depend on the windows the developer
happened to have open; seven reads, `window_parent`, the stickiness check, the space type,
`server_frame`, `key_focused_window`, `get_window_at_point` and the sub-level, had no test answer at
all, and `make_key_window` was carried out.

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
the fake's default answers are the unanswerable ones: `None`, or an empty list. A test
that needs a specific answer says so:

```rust
let ws = Arc::new(FakeWindowServer::default());
ws.set_space_window_list(space.get(), Some(vec![wsid]))
    .set_window_ordered_in(WindowServerId::new(wsid), Some(false))
    .set_window_spaces(WindowServerId::new(wsid), vec![space]);
```

A reactor test states them on the reactor's own fake, `reactor.window_server()`, so what it states
reaches no other reactor, and it never states one unknown again to tidy up after itself. No test
states an answer anywhere but on a fake it holds (`no_test_states_an_answer_through_an_override`).

### How this was found

The reactor test asserted a frame write that never came, and the requests it did
make were only `[GetVisibleWindows]`. Printing the window's workspace at each step
of `handle_authoritative_space_snapshot` put the loss inside
`finalize_space_change`, and printing the authoritative window list showed the
developer's own desktop in it. `std::backtrace::Backtrace::force_capture()` in the
`WindowRemoved` handler named the second one.

## A process static is shared by every test

A `static` is one value for the whole test binary, so in a parallel run a test that reads
one sees whatever a test beside it wrote. A reset when the writer ends is not enough on
its own: the reader can run in the middle. Each static a test writes is **injected at its
reader**, so no test reads the static at all. The reader takes a function, the production one
reads the static, and a test's default answers for it. What one actor tells another goes as an
event, or as a record shared between the two that each test makes its own, rather than a static:
the display churn reaches the reactor as `DisplayChurnBegin`, in the order it handles everything
else, and as the `ChurnReader` of the record the spaces actor writes
(`src/displays/platform/display_churn.rs`), which a test reactor or spaces actor gets fresh
(`two_records_never_see_each_other`).

| static | tests that write it | tests that read it | held by |
|---|---|---|---|
| `LAST_WINDOWSERVER_ACTIVITY_US` (`src/windows/platform/window_server.rs`) | two spaces actor tests, through `note_windowserver_activity`; no reactor test, whose reads note on a clock of their own (`a_window_event_is_noted_on_the_reads_activity_clock`) | `the_production_reader_reports_a_window_server_that_just_moved_as_busy`, which only asks whether it is busy right after setting it; the spaces actor reads `LiveDisplays::window_server_quiet` | injection; the one reader holds because every writer moves the clock later |
| `LOW_POWER_MODE` (`src/app/power.rs`), `MOUSE_STATE` (`src/windows/platform/mouse.rs`) | none: only the notification handler and the event tap write them | the input tap's start reads the initial value; the reactor reads them through `SystemReads`, which a test reactor answers not in Low Power Mode and the pointer unknown; the app actor reads `MOUSE_STATE` only through `AxWorld::mouse_state`, which `FakeAx` answers | injection at the reactor and the app actor; a test that writes one injects it first |
| `SAVE_TEMP_COUNTER` (`src/workspaces/engine/persistence/mod.rs`) | persistence tests | none read its value; it only makes temp names unique | nothing needed |

The test that holds this: `display_churn_waits_for_a_quiet_window_server_as_its_reader_reports_it`.
The production reader behind its injection is held by
`the_production_reader_reports_a_window_server_that_just_moved_as_busy`.

## The suite runs in parallel, and a Core Animation read-back holds its lock

`cargo test` runs on libtest's default threads, one per core: `.cargo/config.toml` sets no
`RUST_TEST_THREADS`. On 2026-10-15, on 12 cores, the lib suite's 2770 tests took 8.9s to 9.7s
in each of 20 consecutive `cargo test --workspace` runs, all green, where one thread took 54.7s.

One kind of test is serialised: a test that installs an animation on a layer and reads it back.
A probe installed one animation 300 times per row and read each back 50 times, while a second
thread did the row's work:

| the second thread | lost | lost, holding `CATransaction::lock` |
|---|---|---|
| nothing | 0 | 0 |
| commits a `CATransaction` that changes another layer | 300 | 0 |
| changes another layer, then `CATransaction::flush` | 300 | 0 |
| a thread that changes a layer and exits | 300 | 0 |
| a thread that makes a layer, changes nothing and exits | 0 | 0 |

A commit on any thread, the implicit one a thread commits as it exits included, drops every
animation from a layer that no render context holds, whatever its begin time. An explicit
transaction around the install and the read-back kept nothing either (300 lost);
`removedOnCompletion = false` keeps it. So a `CATransaction` nests per thread, but what its commit
drops is not per thread. A test's layers are in no render context, and every test that changes a
layer commits when its thread ends, so a read-back beside one found nothing: 11 to 15 failures per
parallel run, "nothing under {key}", in `animation::platform::overlay` and
`decorations::platform::layers`.

`decorations::platform::layers::hold_core_animation` takes Core Animation's own lock for the
test's lifetime. No commit landed while the probe held it, so a commit elsewhere waits for the
read-back instead of landing inside it. Every read-back helper calls `assert_core_animation_held`, so a test
that reads back without the hold fails on its first run, not on an unlucky one. 22 tests hold it;
`a_commit_on_another_thread_drops_a_test_layers_animation_once_the_hold_is_let_go` holds the
behaviour it rests on.

The suite used to run on one thread for another reason, an abort while SkyLight starts up lazily
on two threads at once:

```
objc[75992]: Cannot form weak reference to instance of class
SLSWindowManagementFallbackBridge. It is possible that this object was
over-released, or is in the process of deallocation.
```

Its crash report puts every thread in `SLSWMBridgeDelegate`, which SkyLight sets up on the first
window-management call, `SLSCopyBestManagedDisplayForRect` here: four `ScreenCache` tests made
that call at once, through the Dock read `refresh` made itself. A test that reads the machine is
the cause, so the cure is the seam: `ScreenCache` now asks its `System` for the menu bar, the Dock
and each display's space, and its tests read no SkyLight. On 2026-10-16 the lib tests aborted in 4
of 10 runs before that and in 0 of 20 after, and `bin/check.sh` in 0 of 20 consecutive runs
after. If it comes back, the crash report in `~/Library/Logs/DiagnosticReports` names the tests
whose threads are in SkyLight. The tests that build capture fixtures avoid `IOSurface` for the
same reason, and use a CPU bitmap instead.

## What a flight costs is measured on the screen, by the timing battery

The suite cannot say whether a change slows a flight or flies a blurred window flat: that takes the
live service, the window server and Kai's windows. The timing battery does, and any change to what
a flight or the rest windows do at a show, in flight or at a lift is measured with it before it is
called done (`specs/animation.md`, "Correctness the user can see"). It is a screen test: it posts key
presses, so it runs only while Kai is away, and it refuses otherwise.

```bash
tools/focuslab/battery.sh --check                         # the windows it would fly; posts nothing
tools/focuslab/measure.sh <signed binary> <prefix>        # deploy, then ring off and on, 1 warm-up + 3 each
python3 tools/focuslab/compare.py /tmp/rini-focuslab/<prefix>-off-{1,2,3} -- /tmp/rini-focuslab/<prefix>-on-{1,2,3}
```

What it presses and records, the 87 metrics, the acceptance rule and its second stage are in
`tools/focuslab/README.md` ("The battery"). Its tests (`tools/focuslab/test_battery.py`) run inside
`tests/focuslab.rs`, so `cargo test` counts them as one test. The runs so far and their verdicts are
listed there too.

## How a resize looks is watched on the screen, by a recording of the lab

The suite pins where a resizing tile's content stands and where its ground goes. Whether it reads
right on the screen, its content against the edges that move less, its ground and the swap to the
real window at the lift, is seen only on a film of the whole display, which the battery does not
take. `tools/focuslab/screenrec.sh` takes one of Ghostty lab windows full of a numbered ruler
while posting the resize keys, and `sheet.sh` cuts each press into a contact sheet
(`tools/focuslab/README.md`, "Watching a resize"). It is a screen test with the battery's guard, run
only while Kai is away. The recordings so far are listed there too.

## Clippy is a check, at zero warnings

The checks a change passes are `bin/check.sh`: the nightly format check, a build of every target
with warnings as errors, `cargo clippy --workspace --all-targets -- -D warnings`, and the suite,
`tests/architecture.rs` with it. A clippy warning fails the check as a compiler warning does.

CI checks and releases on Rust 1.98.1, pinned in both workflows, because a new stable can add a lint
that turns `-D warnings` red with no change to the code. Moving the pin is a change of its own, with
whatever the new lints ask, held by `ci_installs_clippy_with_the_pinned_toolchain` and
`every_workflow_installs_the_pinned_rust`. The local check runs whichever `cargo` is on `PATH`, so it
agrees with CI only at the same version.

Every crate takes its lints from `[workspace.lints]` in `Cargo.toml` (`[lints] workspace = true`,
held by `every_crate_inherits_the_workspace_lints`): clippy's defaults, plus `redundant_lifetimes`,
`unused_macro_rules`, `fn_params_excessive_bools` (denied, with `max-fn-params-bools = 1` in
`clippy.toml`), `struct_excessive_bools` and `allow_attributes_without_reason`. The last means every
`#[allow]` says why, in one of two ways. One a roadmap task removes names the quality roadmap's task
id (`reason = "P5.1"`) and sits on the one item it covers, and only when that task's done-criterion
takes the lint away: splitting a function or a file leaves its parameter lists as long. One allowed
for good names what makes it right, such as a C name, a reference count Core Foundation drives too,
a `RefMut` dropped before the await clippy counts it across, or a struct whose `bool` fields each
stand on their own. A long parameter list, a tangled type or a run of `bool` parameters is named
as a struct or an enum rather than allowed
(`no_long_parameter_list_tangled_type_or_bool_run_is_allowed`).

An allow over a whole crate or file, `#![allow]`, is one of these, held by
`every_file_wide_allow_is_named_in_docs_testing`:

| File | Allows |
|---|---|
| `crates/rini-mach-sys/src/lib.rs` | Mach's C names; unsafe operations and `missing_safety_doc`, P5.1 |
| `crates/rini-ipc/src/mach.rs` | Mach's C names; unsafe operations and `missing_safety_doc`, P5.1 |
| `crates/rini-skylight-sys/src/lib.rs` | SkyLight's C names |
| `src/displays/platform/cgs_notify.rs` | SkyLight's C names |
| `src/windows/platform/sub_level.rs` | SkyLight's C names |
| `src/windows/platform/app_actor/subscriptions.rs` | Accessibility's notification names |
| `src/windows/platform/carbon.rs` | `missing_safety_doc`, P5.1 |

The quality roadmap the ids come from is a plan kept outside the repo, so the row of every id an
allow cites is copied here. `every_roadmap_id_an_allow_cites_has_a_row_in_docs_testing` holds the
table to the ids the allows cite, both ways, so a row goes with the last allow that cites it.
Nothing else in the repo cites an id; a comment or a doc names the work in words
(`a_roadmap_id_is_cited_only_in_an_allow_s_reason_or_docs_testing`):

| Id | Task | Done when |
|---|---|---|
| P5.1 | SAFETY comments everywhere; deny `undocumented_unsafe_blocks` and `unsafe_op_in_unsafe_fn`; replace `static mut SERVER` in `rini-ipc` | clippy clean with both denied; 0 `static mut` in the repo |

`unsafe_op_in_unsafe_fn` is not in `[workspace.lints]`. Edition 2024 warns on it already, and
`rini-mach-sys` and `rini-ipc`'s `mach.rs` allow it with 47 unsafe operations outside an `unsafe`
block, so P5.1 denies it once they are wrapped, with `undocumented_unsafe_blocks` beside it.
