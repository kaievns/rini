# `windows` — windows and the apps that own them

The most upstream feature. It knows what a window is, which application owns it, and
what rini is allowed to do to it. It knows nothing about screens, spaces, workspaces
or layout: everything else depends on it and it depends on nothing.

## What it owns

| | |
|---|---|
| **Which apps have a thread** | `domain/app_threads.rs` — when a running application gets its Accessibility thread: launched, observed ready, seen ready when the window server reports a new window of it, or seen ready when it next becomes frontmost; and never a second one |
| **The catalogue** | `domain/catalogue.rs` — every window rini has seen, keyed by `WindowId { pid, idx }`, with its window-server id and what that id is seen doing (visible, observed, on which space), the last rule decision, and the native-fullscreen records that let a window come back |
| **What a window IS** | `domain/info.rs` (`WindowInfo`, `AppInfo`, `WindowServerInfo`, and `reported_radius`, the one corner radius a window rounds at from the four the window server reports) and `domain/state.rs` (`WindowState`, the reactor's record of a window and whether it is managed) |
| **Whether rini manages a window** | `domain/manageable.rs` — `manageable(Facts)`, over what Accessibility says (minimized, standard, top-level) and what the window server says (layer, sticky, level), an unanswered window-server fact turning no window away, and the window server asked nothing more once `turned_away_before_accessibility` says the facts in hand turn a window away (`no_read_is_made_once_the_facts_in_hand_turn_the_window_away`); and `looks_gone`, whether a window the app no longer lists is gone, only on an explicit negative observation |
| **A window's place in the stack** | `domain/stack.rs` — `StackPlace`, a window's frame, level and sub-level; `covered_by_peer_above`, whether raising a window would put it over a peer the user has on top inside it; and `stack_place_writes`, the level and sub-level writes that put a window of rini's own in another's place |
| **A window's home space** | `domain/home_space.rs` — of the spaces the window server places a window on, the first user space, else the first: a window mid-fullscreen-transition is on its Desktop and a fullscreen space at once |
| **The AX boundary's rules** | `domain/ax_events.rs` — whether a failed Accessibility request means the window is gone, and the codec for the one `usize` an observer callback carries |
| **What rini takes on** | `domain/admissible.rs` — the four reasons an Accessibility window is turned away before anything else knows it exists |
| **Window rules** | `domain/rules.rs` — every `window-rule` that applies to a window, folded in file order as niri does, and the one matcher (`applies`) that the rules for what a window wears (`decorations::domain::rules`) share, which reads whether a window holds focus, floats or is drawn urgent (`RuleState`) beside what it is. Resolving the result against workspaces is `workspaces::domain::app_rules` |
| **Focus** | `domain/focus.rs` — `MainWindowTracker` and the key window it has heard of, which windows' looks a change of it turns, the raise-echo rule, and the activation-focus rule that stops cmd-tab moving a display |
| **Focus order** | `domain/focus_order.rs` — `FocusOrder`, the windows in the order they were last focused, most recent first, which the switcher offers them in |
| **Raising** | `domain/raise.rs` — the raise manager |
| **Raise order** | `domain/raise_order.rs` — which windows are worth raising, and in what order; `AppBatch`, the windows one application's thread raises together, which no other application's window joins, batched by application and space in the order they came (`group_by_app_and_space`); `StackGroup`, on the strip or off it, and `regroup_tiled`, which raises the strip back in front of a window off it that sits inside it |
| **Frame transactions** | `domain/transaction.rs` — txids, so a frame report can be matched to the write that caused it |
| **Frame writes** | `domain/frame_write.rs` — which Accessibility writes a frame takes: the position alone when the window server already has the window at the desired size, size, position and size again otherwise, and again after a position written alone that read back at another size |
| **Title-bar zoom** | `domain/zoom.rs` — which window's title bar a double-click landed on, and what that window's resizes in the next second mean: the zoom, which toggles full width once, or the rest of it, which rini overrules |
| **The port into a window** | `domain/request.rs` — `Request`/`Quiet`, what the per-app thread accepts |
| **Attention** | `domain/attention.rs` — which applications ask for attention (`Attention`), per application as macOS asks it |

Two questions about raising and admission that were closed on evidence rather than code are in
`admission-and-raise-findings.md`.

`platform/` is the macOS side: `app_actor.rs` is the per-app Accessibility actor (one thread per
application, observing and writing), `window_server.rs` the window server through SkyLight, behind
the `WindowServer` port (below), `lifecycle.rs` the
launch, quit and front-app listener over `carbon.rs`'s Carbon events, `app.rs` the activation-policy
and finished-launching observers, `process.rs` the process-type check, `sub_level.rs` the SkyLight
sub-level port, `attention.rs` the thread that reads from LaunchServices once a second which
applications ask for attention.

## The shape that matters

**One thread per application.** An AX call blocks on the owning app, so a hung
application must not hang rini. Each app gets a thread with its own `CFRunLoop`, and
the reactor talks to it through `Request`. An application slow to answer while it
launches is asked again rather than given up on, by the thread's launch loop on
`platform/ax/ask_again.rs`'s schedule; a window it was too busy to subscribe is taken
on at the next AXWindows refresh. The observer hands an application too busy to
answer back as an error, and asks nothing again itself. One that becomes frontmost
without a thread is looked at again then (`launch-notifications.md`). The thread
ends with the application: the reactor's `Terminate` at a quit, or the first request
that fails because the application has gone, ends its task, and a raise queued or
under way is dropped with it rather than kept waiting on an application that will
not answer. The observer, the elements and the window map go with the thread.

**A window has two identities.** `WindowId { pid, idx }` is rini's own and dies with
the process. `WindowServerId` is macOS's, survives, and is recycled. The catalogue
keys by the first and indexes by the second, which is why a rekey is an operation
rather than an assignment.

**Accessibility and the window server disagree.** AX reports a window before the
server has it, so `admissible::has_visible_peer` treats "no server id yet" as visible
rather than absent. The reverse — an id the server does not report — means the window
is not on screen.

**Reads go on a connection of their own.** A window-server read that waits on the last Core
Animation transaction of the connection it is given (a window query, a SkyLight capture) is made on
`READ_CONNECTION`, which no window of rini's belongs to. On the main connection it took the
connection's lock and waited for the commit of rini's own windows that the main thread, waiting for
the same lock, was making: both stopped 500ms, until SkyLight timed the wait out. A read that waits
on the main connection whatever it is given (`CGWindowList`, the on-screen list, a space's window
list, whether a window is ordered in) is not made: the same answer is read another way. Writes to rini's own windows stay on the main connection, which owns
them. Each call, which connection it takes and why, and the measurements: `window-server-reads.md`.

**The window server is a port.** Every query and command a rule or an actor outside
`window_server.rs` makes of the window server is a method of the `WindowServer` trait
(`platform/window_server/port.rs`): the windows and their frames, parents, levels, sub-levels,
spaces, order, suitability and corners, the space lists, the depths, the window under the cursor and
at a point, the key window and process, the listings, and the two commands, `make_key_window` and
`take_stack_place`. A consumer holds an `Arc<dyn WindowServer>`, cloned to each thread that asks:
production passes `window_server::live()`, which is `LiveWindowServer`, the SkyLight calls, and
which panics in a test build (`a_test_build_never_takes_the_live_window_server`); a test
passes a clone of an `Arc<FakeWindowServer>` (`platform/window_server/fake.rs`) and keeps the other
to state answers (`set_window_ordered_in(id, Some(false))`, one `set_` per fact) and to read the
commands it was given (`commands()`). A read the live window server answers from another read's
answer is answered from the same stated fact, so a test cannot state a pair production never
produces: stickiness from the spaces (`a_window_is_sticky_exactly_when_it_is_stated_on_more_than_one_space`),
the level from the window's layer (`a_window_s_level_is_the_layer_it_was_stated_with`), the depths
from the on-screen list (`the_depths_are_the_stated_on_screen_list_without_the_desktop`), the window
under the cursor from the window at a point (`the_window_under_the_cursor_is_the_window_at_a_point`). A trait object rather than a struct of fn pointers like
`SpaceKinds`, because the fake keeps what a test stated and what it was told, which a fn pointer
cannot capture. The fake answers "unknown", `None` or an empty list, to every read nothing was
stated for, and only records a command (`every_read_answers_unknown_with_no_setup`,
`make_key_window_and_take_stack_place_only_record`): a negative answer retires a window, so a fake
must never guess one. The rules over the answers take them as values (`domain/manageable.rs`,
`domain/stack.rs`, `domain/home_space.rs`), so a rule's test needs no port at all.

The reactor holds its port inside `app::system_reads::SystemReads`, with its other reads of the
machine. Callers not yet handed a port call the free functions of the same names in `window_server`, which
`platform/window_server/shim.rs` answers from `LiveWindowServer` in production and from a
thread-local `FakeWindowServer` in a test build, stated through the old `set_*_override` setters.
That shim is temporary: the flight engine
(`the_flight_engine_reads_the_window_server_only_through_its_port`) and the spaces actor are handed a
port, and the app actor is on it through its `AxWorld`'s `server()`, `live()` in `MacAx` and a
`FakeWindowServer` in `FakeAx`, asking the pointer state and posting Command-W through the world too,
so it makes no platform call past it and its tests take no path through `window_server` or `mouse`,
`MouseState` coming from the world (`the_app_actor_reaches_the_platform_only_through_its_world`).
Once its last callers, the platform code of the frames, pictures, rest-border, bar and input
threads, are handed a port, it goes. Through the shim, a window no test stated is at the origin,
800x600; on the port it is unknown.

**A raise list is a z-order.** Raising is last-wins, so the order of the list IS the
stacking it produces. `domain/raise_order.rs` exists because that order kept being
thrown away: the batching step grouped the list through a hash map, which handed the
batches back in hash order and undid the strip regroup that had just been built. With
eight applications, `FxHashMap` returned them exactly reversed, so the focused window
was raised first instead of last and ended up behind the strip.

## Reading order

`domain/info.rs` → `domain/catalogue.rs` → `domain/admissible.rs` → then
`platform/app_actor.rs` for how the AX side drives it.

## Detail

- [`multi-display-focus.md`](multi-display-focus.md) — why rini keeps its own record
  of each app's focused window, and how cmd-tab is told apart from cmd-`
- [`launch-notifications.md`](launch-notifications.md) — what macOS reports while an
  application launches, measured, and how to read a launch back out of the unified log
- [`attention.md`](attention.md) — what shows an application asking for attention, measured, and
  what the attention reader reads and costs
- [`window-server-reads.md`](window-server-reads.md) — which window-server reads wait on Core
  Animation, the 500ms lock that made them stall, and the connection each read takes

Cross-feature detail is at the top of the tree: [`docs/architecture.md`](../../../docs/architecture.md),
[`docs/testing.md`](../../../docs/testing.md).
