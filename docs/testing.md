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
none. Each boundary has one seam but the window server, which has three: `WindowServer`; the frames
thread's narrower `Reader`, until that thread moves onto the port; and the pictures thread's
`Capturer`, which makes its captures as well as its reads.

| what the code asks | seam | production | a test passes |
|---|---|---|---|
| the window server: windows, frames, levels, spaces, order, the space lists, the key window, the listings; `make_key_window`, `take_stack_place` | `WindowServer`, an `Arc<dyn WindowServer>` (`src/windows/platform/window_server/port.rs`) | `window_server::live()` | a clone of an `Arc<FakeWindowServer>`, answers stated with its `set_` methods, commands read back with `commands()`, reads counted with `reads()`; every read unknown until stated |
| Accessibility, from the per-app thread | `AxWorld`, a type parameter (`src/windows/platform/ax/world.rs`) | `MacAx` | `FakeAx`, whose elements are numbers; an element with nothing recorded answers as a window that has gone |
| the window server, from the frames thread that follows windows | `Reader`, a struct of fn pointers (`src/displays/platform/window_notify.rs`), a narrower view of the window server answered through the same free functions, which folds into `WindowServer` when `window_notify` moves onto the port | `LIVE` | a `Reader` of its own fns |
| displays and spaces, from the spaces actor | `LiveDisplays` and `SpaceKinds`, structs of fn pointers (`src/displays/platform/spaces.rs`) | `from_window_server()` | `LiveDisplays::silent()`, which knows nothing, and `SpaceKinds::for_tests()`, or a struct of its own |
| the window server and ScreenCaptureKit, from the pictures thread | `Capturer`, a type parameter (`src/pictures/platform/capturer.rs`) | `MacCapturer` | `FakeCapturer`, answering from the frames a test gives it and recording every call |
| the screens, for `ScreenCache` | `System`, a type parameter (`src/displays/screen.rs`) | `Actual` | a `System` of its own, as the stubs in `screen.rs`'s tests are |

A caller not yet handed a `WindowServer` calls the free functions of the same names in
`window_server`, answered in a test build by a thread-local `FakeWindowServer` the `set_*_override`
setters state answers into. That shim is temporary, and nothing new is written against it: the
reactor, the flight engine, the app actor and the spaces actor move onto a port and it goes.

## A unit test must not read the live window server

A test asks the window server through `FakeWindowServer`, handed to the code under test as its
`WindowServer` port, or, for code not yet on the port, through the shim's thread-local one. Every
read of the fake answers "unknown" until the test states an answer
(`every_read_answers_unknown_with_no_setup`), and a command is recorded, never carried out
(`make_key_window_and_take_stack_place_only_record`), so no test reads the live window server or
makes a real window key (`a_test_never_reads_or_drives_the_live_window_server`). The live port is
out of a test's reach too: `window_server::live()` panics in a test build and nothing outside
`window_server` builds a `LiveWindowServer` (`a_test_build_never_takes_the_live_window_server`), so a
test that builds a production reactor or actor through it fails rather than reading SkyLight. Before the port,
`window_server.rs` answered from thread-local overrides under `cfg(test)`, and four of those fakes
fell through to the real window server when no override was set, which made reactor tests depend
on the windows the developer happened to have open; seven reads, `window_parent`, the stickiness
check, the space type, `server_frame`, `key_focused_window`, `get_window_at_point` and the
sub-level, had no test answer at all, and `make_key_window` was carried out.

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

The shim's `set_*_override` setters state the same answers into its thread-local fake, which
lasts as long as the thread that set it, and libtest runs every test on a thread of its own,
at one test thread as at many. A probe of two tests at `RUST_TEST_THREADS=1`: `a` ran on
`ThreadId(2)` and set a thread-local to 7, `b` ran on `ThreadId(3)` and read 0. So no override
reaches another test, and the `*_override(.., None)` calls at the end of tests do nothing for
isolation. They are left in place: a reset part-way through a test does change what its next
step reads, and telling those apart from the trailing ones is site by site.

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
its own: the reader can run in the middle. Each static a test writes is held one of two
ways:

- **Injected at its reader**, so no test reads the static at all. The reader takes a
  function, the production one reads the static, and a test's default answers for it.
- **Held for the test's lifetime** by a guard modelled on
  `display_churn::hold_for_test`: a mutex, so one such test runs at a time, and a reset in
  `Drop`, so the value is clear when the test ends, pass or fail
  (`a_test_that_panics_holding_the_flag_leaves_it_clear`).

| static | tests that write it | tests that read it | held by |
|---|---|---|---|
| `DISPLAY_CHURN_ACTIVE` (`src/displays/platform/display_churn.rs`) | spaces actor tests, through `begin` and `end` | `a_production_reactor_is_quarantined_while_the_process_wide_churn_flag_is_set`, under `hold_for_test`; every other reactor reads its own `display_churn` field, false under test | `hold_for_test` around every writer and that reader |
| `LAST_WINDOWSERVER_ACTIVITY_US` (`src/windows/platform/window_server.rs`) | every reactor test that handles a window event | `the_production_reader_reports_a_window_server_that_just_moved_as_busy`, which only asks whether it is busy right after setting it; the spaces actor reads `LiveDisplays::window_server_quiet` | injection; the one reader holds because every writer moves the clock later |
| `LOW_POWER_MODE` (`src/app/power.rs`), `MOUSE_STATE` (`src/windows/platform/mouse.rs`) | none: only the notification handler and the event tap write them | the reactor and the app actor read the initial value | nothing yet; a test that writes one injects it first |
| `SAVE_TEMP_COUNTER` (`src/workspaces/engine/persistence/mod.rs`) | persistence tests | none read its value; it only makes temp names unique | nothing needed |

The tests that hold this: `a_test_reactor_is_not_quarantined_by_the_process_wide_churn_flag` and
`display_churn_waits_for_a_quiet_window_server_as_its_reader_reports_it`. The production readers
behind both injections are held by
`a_production_reactor_is_quarantined_while_the_process_wide_churn_flag_is_set` and
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

The suite used to run on one thread for another reason, an abort while macOS frameworks start up
lazily on two threads at once:

```
objc[75992]: Cannot form weak reference to instance of class
SLSWindowManagementFallbackBridge. It is possible that this object was
over-released, or is in the process of deallocation.
```

On 2026-10-10 it took a few percent of 40 parallel runs. It took none of the 47 parallel runs
on 2026-10-15, with nothing changed that it was traced to; if it comes back the message names it.
The tests that build capture fixtures still avoid `IOSurface` for that reason, and use a CPU
bitmap instead.

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
`unused_macro_rules` and `allow_attributes_without_reason`. The last means every `#[allow]` says
why, in one of three ways. One a roadmap task removes names the quality roadmap's task id
(`reason = "P3.8"`) and sits on the one item it covers, and only when that task's done-criterion
takes the lint away: splitting a function or a file leaves its parameter lists as long. One allowed
for good names what makes it right, such as a C name, a reference count Core Foundation drives too,
or a `RefMut` dropped before the await clippy counts it across. One no task removes says so: a long
parameter list no task groups carries `reason = "no roadmap task groups them"`.

An allow over a whole crate or file, `#![allow]`, is one of these, held by
`every_file_wide_allow_is_named_in_docs_testing`:

| File | Allows |
|---|---|
| `crates/rini-mach-sys/src/lib.rs` | Mach's C names; unsafe operations and `missing_safety_doc`, P5.1 |
| `crates/rini-ipc/src/mach.rs` | Mach's C names; unsafe operations and `missing_safety_doc`, P5.1 |
| `crates/rini-skylight-sys/src/lib.rs` | SkyLight's C names |
| `src/displays/platform/cgs_notify.rs` | SkyLight's C names |
| `src/windows/platform/sub_level.rs` | SkyLight's C names |
| `src/windows/platform/app_actor.rs` | Accessibility's notification names |
| `src/windows/platform/carbon.rs` | `missing_safety_doc`, P5.1 |

The quality roadmap the ids come from is a plan kept outside the repo, so the row of every id an
allow cites is copied here. `every_roadmap_id_an_allow_cites_has_a_row_in_docs_testing` holds the
table to the ids the allows cite, both ways, so a row goes with the last allow that cites it. Nothing
else in the repo cites an id; a comment or a doc names the work in words
(`a_roadmap_id_is_cited_only_in_an_allow_s_reason_or_docs_testing`):

| Id | Task | Done when |
|---|---|---|
| P3.8 | `EngineFixture` over the production layout fn; delete the `cfg(test)` `calculate_layout` and the test-only `create_workspace`/`test_app_rules` | engine test average under 30 lines; `virtual_workspace.rs` has one `cfg(test)` |
| P4.1 | Named structs for positional tuples: the `OnScreenEntry` 9-tuple, `Request::SetWindowFrame(.., bool)`, `Event::WindowFrameChanged`, `ReactorParts` for an 11-parameter `spawn` | no tuple over 3 in an enum variant or pub fn type at these sites |
| P4.8 | Small enums: `Pass` for `update_layout(bool, bool, ..)`, `Press`, `Axis`, the repaint subsets in `StageSummary`/`Flight` | no fn with 2+ bool parameters (`clippy::fn_params_excessive_bools` denied) |
| P5.1 | SAFETY comments everywhere; deny `undocumented_unsafe_blocks` and `unsafe_op_in_unsafe_fn`; replace `static mut SERVER` in `rini-ipc` | clippy clean with both denied; 0 `static mut` in the repo |

`unsafe_op_in_unsafe_fn` is not in `[workspace.lints]`. Edition 2024 warns on it already, and
`rini-mach-sys` and `rini-ipc`'s `mach.rs` allow it with 47 unsafe operations outside an `unsafe`
block, so P5.1 denies it once they are wrapped, with `undocumented_unsafe_blocks` beside it.
