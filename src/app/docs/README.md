# `app` — the application layer

What turns the features into rini. Nothing in a feature imports this; the application
knows the features and converts what they emit.

## What it owns

| | |
|---|---|
| `reactor/` | The orchestrator. Holds every feature's store, reduces the events they emit, drives them back |
| `reactor/mod.rs` | `Reactor` and its `Event`: the loop that takes each batch, and `apply_event_outcome` carrying out what the reducer left, in order |
| `reactor/dispatch.rs` | `dispatch_workflow`: the focus report an event carries taken in, then the event handed by its family (applications, windows, spaces and the system, input and the bar, commands) to its reducer |
| `reactor/active_spaces.rs` | Which spaces rini is active on, recomputed from the screens, and each window's visibility and space reconciled against the active spaces' window lists |
| `reactor/space_snapshot.rs` | A snapshot of the displays' spaces taken in: screens and spaces replaced, a departing display's windows given it as their home, the topology's window moves, windows back from a native fullscreen space, and the snapshot held while Mission Control is open |
| `reactor/display_homes.rs` | Which display each window belongs on: the arrangement in force, display pins, homes observed from the live layout, windows moved back to a returning display or by the redistribute command |
| `reactor/window_refresh.rs` | The applications asked for their windows again, and what they report taken in: window-server info applied, stale windows cleaned up, the refresh after Mission Control |
| `reactor/layout_pass.rs` | A layout event handed to the engine and its response carried out, and the layout pass after it, a `Pass` that is plain, a resize's or a workspace switch's: every active space laid out, a scrolled-away column parked at a 1pt sliver (`bound_frame_to_screen`), and each space handed to the animation |
| `reactor/strip_motion.rs` | The strip's motion on the overlay: overlay frames placed, a pan along the strip, an edge bounce, a workspace switch, and what the transitions engine is told about the display and its journal |
| `reactor/regroup.rs` | The strip kept one group in front of the floating windows after focus lands on it (`strip_group_to_lift_for`), and each raise sent with the focus reports it will provoke |
| `reactor/focus_heuristics.rs` | Where focus goes when nothing names it: the window under the cursor, an empty workspace's desktop, the window a workspace switch focuses (`choose_switch_focus`), raise on mouse over, focus follows mouse |
| `reactor/activation.rs` | An application activating: whether its window's workspace is switched to, and which of its windows takes focus when macOS picks a parked one |
| `reactor/app_rules.rs` | The app rules carried out: windows assigned by their rules as they appear, as their space activates and as a title changes, and the placements and resizes a rule asks for |
| `reactor/drag_swap.rs` | A window being dragged: the drag session, and the swap with the window it is dragged over |
| `reactor/switcher.rs` | The window switcher's session: the list snapshotted at open, the cursor over it, the popup, and the commit that focuses the chosen window wherever it is |
| `reactor/command_space.rs` | Which space and screen a command acts on: the command space, the active display's space, the focused or main window's, and the screen a display selector names |
| `reactor/autosave.rs` | The layout file written: debounced after a layout change, at once before exit |
| `reactor/state.rs` | `RiniState` |
| `reactor/events/` | The reducers, one module per event family |
| `reactor/events/app.rs` | Applications launching, terminating, losing their thread, activating, and the windows each one reports |
| `reactor/events/command.rs` | The commands: layout commands, closing a window, a config update, saving before exit, and focusing or moving the mouse to a display |
| `reactor/events/drag.rs` | A mouse-up ending a drag: the swap it deferred carried out, and a window dragged onto another display's space moved to that display's strip in its own workspace |
| `reactor/events/focus.rs` | `resolve`: the space and window a workflow's focus request lands on |
| `reactor/events/outcome.rs` | `EventOutcome`: what a reducer leaves for after its mutation, in order (window-server updates, discoveries, app requests, raises and refreshes), `absorb`, which merges two, `absorb_ahead`, which takes in what is left of work handed back mid-drain, and `take_work_before_arrange`, which splits off what goes before the arrange and keeps whether it focused a window |
| `reactor/events/space.rs` | The window server's windows appearing and going, native Mission Control entered and left, and space lifecycle, with the fullscreen departure and last-known user space rules |
| `reactor/events/system.rs` | Menus opening and closing, waking from sleep, a raise completing or timing out, and the WM controller's sender registering |
| `reactor/events/window.rs` | One window's events: created, destroyed, minimized and back, its frame changing (`classify_window_frame_change`), its title, and the mouse moving over it |
| `reactor/events/window_discovery.rs` | An application's window list: which known windows are stale and leave, which are new, and the layout events they make |
| `reactor/commands.rs` | What a command MEANS before it is carried out: which window "index 3" is, which display "next" is |
| `reactor/observations.rs` | The other half of the inbox: what the system reported, gathered and handed over settled |
| `reactor/space_affinity.rs` | Which space a window is on — the gathering |
| `reactor/space_resolution.rs` | Which space a window is on — the rules, pure |
| `reactor/quarantine.rs` | What is held back while displays or the session are unstable: events dropped during display churn, as the spaces actor's `DisplayChurnBegin` and `DisplayChurnEnd` bound it, and while refreshes are blocked, and the one visible-window refresh owed until every quarantine lifts |
| `reactor/query.rs` | Answers from a borrowed `StateView` |
| `reactor/present.rs` | The frame-writing capability: record a destination, number the write; and `instant_requests`, what a pass that does not fly sends one application |
| `reactor/bar.rs` | The bar's half here: its model built from the stores after every batch and sent only when it changed, and what a click on it asks for |
| `reactor/managers.rs` | The state the reactor keeps per concern in more than one field: the drag, Mission Control, the workspace switch, refocus, the refresh quarantine and the channels to the other actors; and `notification_ids_to_publish`. The quarantine is three machines, `Wake`, `Session` and `Churn` (`Churning` drops window events, `Settling` still holds refreshes until a snapshot), and the one refresh owed, all moved only by `RefreshQuarantineManager::on` and its table tests |
| `reactor/animation.rs` | `animate_layout`: whether each layout pass flies on the overlay, and placing the real windows when it does not, from `transitions::domain::pass`'s plan |
| `reactor/decorations.rs` | The decorations' half here: what each managed window wears as its window rules have it, who wears the ring, each tiled window's border, and what the rest windows are told after a batch |
| `reactor/diagnostics.rs` | What a query answers about when it was not told (`default_query_space`), and what the diagnostics dump counts as wrong (`orphaned_windows`, `stale_display_homes`) |
| `reactor/testing.rs` | Built only for tests: the reactors, screens and space-state events the reactor's tests start from, and the accessors they read and arrange the reactor through |
| `reactor/tests/` | The integration tests, one file per subject, over `reactor/tests/fixtures.rs` |
| `config/` | `config.kdl`: `config/kdl/` decodes each section into a part type and `kdl/apply.rs` lays the parts over each feature's settings, with the checks that span sections; then watching and reload. The format's decisions are [`docs/config.md`](../../../docs/config.md) |
| `api/` | The Mach IPC backend bound to the reactor, and the shapes a query answers in |
| `hotkeys/` | The controller: app launches and which apps get a thread (the rule is `windows::domain::app_threads`), hotkey registration, config reload. `hotkeys/lower.rs` is the translation from a bound `Action` to a `reactor::Command`, which is where the tests are |
| `notifications.rs` | The NSWorkspace demultiplexer |
| `power.rs` | Low Power Mode, read at start and kept current from its notification: the input tap and the reactor's flight decision read it |
| `system_reads.rs` | `SystemReads`, everything the reactor asks of the machine as one value: the window-server port, through which it makes a display's desktop key, the pointer, Low Power Mode, the displays' bounds, the space kinds, the running applications, and the activity clock and window notifications it hands back. `Reactor::new` builds the live one; a test reactor's knows nothing until the test states it on `reactor.window_server()` or swaps a field |
| `boot.rs` | What the flags and the config file say before anything is built: the command line, `--no-animate` over the config, and the config-or-defaults fallback |
| `launch_agent.rs`, `startup.rs`, `logging.rs`, `channels.rs` | The service plist, configured startup commands, tracing, and the span-carrying channel every actor is wired with |

## The shape that matters

**Features talk upward by emitting events.** `windows::event::Event` becomes
`reactor::Event` through a `From` impl. A feature never names `crate::app`, and
[`tests/architecture.rs`](../../../tests/architecture.rs) enforces it.

**The reactor is one actor on one thread.** Everything it holds is single-threaded
state; the actors it talks to have their own run loops and their own threads.

**A broken config must not stop rini starting.** One mistyped binding used to take the
window manager down. It now reports and falls back to the defaults, which is the only
state a user can recover from. A missing `config.kdl` with a `config.toml` beside it is
reported the same way, since the defaults are not what that file configured. A broken
reload keeps the running config and says why, in the log and with a mark on every bar.

**Decisions leave, orchestration stays.** Pure feature logic has been moved out to each
feature's `domain/` over several passes. What remains in `reactor/` reads and writes
several stores per event, which is what the application is for: `reactor/mod.rs` is the
loop, `reactor/dispatch.rs` the dispatch, and each other sibling file is one concern's
`impl Reactor`.

**A bound action is a translation, not a side effect.** `hotkeys/lower.rs` turns an
`Action` into a `reactor::Command`, a program to spawn, a config reload, or a named
refusal. It was ninety match arms inside `handle_event`, each ending in a `send`, so
nothing could check that two actions did not lower to the same command; now a table test
names every action's target.

## Carrying out an outcome

`apply_event_outcome` carries out what a reducer left in two parts. First the steps before
the arrange: pictures forgotten, window-server info, the active spaces, the Mission Control
repair and refresh, discoveries, app rules, created windows, confirmed spaces, fullscreen
restorations, topology moves, the frames written ahead of the layout, layout events and
responses, the drag swap, and a drag's release. Then the arrange, once, and what follows
it: raises, focus, the regroup, key windows, mouse warps, the controller's events, a close,
a config, notifications, and the deferred application requests last.

Four steps hand work back: a discovery (`on_windows_discovered_with_app_info`), a
fullscreen restoration, the space change held while Mission Control was open
(`try_apply_pending_space_change`, from the exit's repair and from a snapshot), and a drag's
release (`on_mouse_up`). `settle_ahead_of_arrange` carries out each returned outcome's steps
before the arrange where it is handed back, depth-first, so they run in the order the nested
drain ran them: the held change's layout events reach the layout before the exit's refresh
pass and before a snapshot repatriates
(`a_space_change_held_through_mission_control_is_laid_out_before_the_exit_refresh_pass`).
What is left of each, its arrange and what follows it, is absorbed in the order it was
handed back, and that is absorbed with `absorb_ahead`, ahead of what is left of the event's.
Nothing re-enters `apply_event_outcome` or `handle_event`, so an event has one arrange, one
regroup and one press in the transitions journal: a drag released inside a frame report is
that report's press (`a_drag_released_inside_a_frame_report_is_the_reports_press`). A
layout event that changes geometry still runs its own pass inside `send_layout_event`, so
the window a discovery adds is laid out where the discovery hands it back, ahead of the
frames written ahead of the layout that come after the discovery
(`a_discovery_lays_its_window_out_where_it_hands_it_back`).

Rejected: each of those steps carrying out its own outcome on the spot. That ran a whole
drain inside the event's, so the nested arrange went before the frames written ahead of
the layout, cleared the command's strip move and the workspace switch before the event's
own arrange, forgot what the event's focus raise had lifted so its regroup could lift the
strip again, and journalled a drag's release as a press of its own. Also rejected: absorbing
what is handed back whole and carrying out its steps before the arrange in a later round.
That put the held space change's layout events after the Mission Control exit's refresh,
which reconciles and lays out on its own, so its pass ran over a layout the change had not
reached.

## Where the reactor's tests are

`reactor/tests/` is one module per subject. A failing test names the subject, and the
file you open to change one is the size of that subject rather than of the reactor.

| file | what it covers |
|---|---|
| `displays.rs` | screens arriving and leaving, resolution changes, per-display workspaces, display homes |
| `tiling.rs` | layout passes, strips, columns, folding, resizes, drags, animation |
| `workspaces.rs` | creating and switching workspaces, moving windows, app rules, queries |
| `focus.rs` | focus moves, focus-follows-mouse, raise echoes, cmd-tab, per-app main window |
| `fullscreen.rs` | full width and height on the strip, and macOS taking a window fullscreen off it |
| `spaces.rs` | which space is current, what happens while that is unknown, space membership |
| `lifecycle.rs` | windows and apps appearing, disappearing, dying, and surviving a restart |
| `windows.rs` | which windows rini takes on and which it refuses |
| `bar.rs` | what each display's bar is sent after a batch, and what a click on it does |
| `reads.rs` | what the reactor asks of the machine: only its `SystemReads`, every read unknown in a test until stated |
| `decorations.rs` | what the rest windows are sent after a batch: each window's ring and border, whether it holds focus or asks for attention, and the rules that dress it |
| `fixtures.rs` | the reactors, apps and windows the cases are built on, each handed back as a struct naming its parts (`OneWindow`, `Floating`, ...) |

A case reads and arranges the reactor through the accessors in `reactor/testing.rs`
(`windows()`, `layout()`, `settings_mut()`, `connect_test_transitions()` and the rest), never
through its fields, so a store can move without touching the cases:
`a_reactor_test_reaches_no_field_of_the_reactor` in `tests/architecture.rs` holds it.

`tiling.rs` is not `layout.rs` because a module named `layout` inside `tests` shadows the
`layout` alias the siblings use for `crate::workspaces`.

A rule that can be tested without a reactor does not belong here. `space_resolution`,
`present`, `hotkeys::lower`, `windows::domain::admissible` and `input::domain::pointer`
each left this module and took their tests with them.

## Reading order

`reactor/state.rs` → `reactor/events/` → `reactor/mod.rs` → `reactor/dispatch.rs` → the
concern files beside it, and `main.rs` for how it is all assembled.

## Known debt

The reactor still holds every feature's store, and each concern's methods borrow all of it
rather than the stores they write: 3.2 and 3.3 of
[`docs/implementation-audit.md`](../../../docs/implementation-audit.md), which also says what
its largest functions were split into. No function in `src/app` is past the size limit, and
`no_function_or_file_grows_past_its_limit` in `tests/architecture.rs` holds each to it.
