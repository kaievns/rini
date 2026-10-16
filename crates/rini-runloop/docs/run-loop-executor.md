# The run-loop executor

Every thread that owns a `CFRunLoop` in rini drives one future from it:
`executor::Executor::run` (or `run_main`, which calls `NSApp.run()` instead of
`CFRunLoop::run`, or `run_interactive`). The main thread joins twelve actors on
its one loop; the reactor, the config actor, the config watcher, the input tap,
the pictures service, the flight engine (`transitions`) and each observed
application have their own thread and their own loop.

`run_interactive` first asks for `QOS_CLASS_USER_INTERACTIVE`, the class Apple
gives an app's main thread, and logs the class the thread had, since what a
thread std spawns starts at is not verified. The flight engine runs on it: a
press waits on that thread from the reactor's request to the overlay going up.

## Why not a tokio runtime

The platform is callback-driven on `CFRunLoop`s, and much of it is
main-thread-only: Accessibility observers, `CGEventTap`, `NSWorkspace`
notifications, CoreAnimation, and `NSApp.run()`, without which some events
never fire ([yabai#2680](https://github.com/koekeishiya/yabai/issues/2680)).
A tokio runtime cannot drive those loops and the loops cannot drive tokio, so
the executor is a `WakeupHandle` (a manual `CFRunLoopSource`) that polls the
one future whenever it is signalled. Tokio's `sync::mpsc` is used for channels
because it is runtime-free; nothing else of tokio is.

## `tokio::time` panics here

`tokio::time::sleep` needs a tokio reactor for its timer and panics with
"there is no reactor running" when polled under this executor. It compiles because tokio-stream's
default `time` feature turns tokio's on (`cargo tree -e features -i tokio`). That panic took
the whole window manager down twice: once from the animation frame clock, and
once from the cursor warp poll, which only reached its sleep branch after a
second display appeared and so hid for weeks. `executor::sleep` and
`run_loop::RepeatingTimer` are the run-loop-backed replacements; `timer::Timer`
is the awaitable `CFRunLoopTimer` the application actors use.

## Main-task lifetime

`Executor::run` keeps calling the loop function until the main task completes,
because other code can stop the run loop spuriously. Unwinding out of the task
drops it (`executor_drops_main_task_on_unwind`), which is what lets a panicking
actor thread release its channel senders.

## A wakeup that outlives its thread

A `Waker` can be held on another thread after the executor's thread ends. As that thread ends,
CoreFoundation takes the source out of the run loop's modes but not out of its common-mode items, so
the loop still holds it until the loop is freed, and the last `WakeupHandle`, which retains the loop,
is what frees it (measured on macOS 26.7: the source's retain count falls from 3 to 2 as the thread
ends, and its release runs where the last handle drops). So the handler can be freed on whichever
thread drops the last `Waker`. The handler's closure holds the executor's `Weak`, which
is not `Send`, so `release` drops it only on the thread that made it and leaks it on any other
(`a_handler_is_never_dropped_on_a_thread_other_than_the_one_that_made_it`).

## Message channels

`rini_runloop::channel` is an unbounded tokio mpsc carrying the sender's tracing
span with each message, so a handler's spans nest under the sender's. Unbounded
is deliberate: a bounded channel between threads that also service run loops
would deadlock the main thread on backpressure.

## Timing the main thread's turns

A drain waiting for the main thread (a `drawn` mark's `queued_us` in the transitions journal) says
how long, not behind what. So each turn of the main run loop is timed: `run_loop::TurnObserver`, made
in `src/main.rs` before `run_main`, opens a turn at after-waiting, ordered `CFIndex::MIN` so before any
other observer, and closes it at before-waiting, ordered `CFIndex::MAX` so after Core Animation's
commit observer (order 2000000, inferred, not verified). A busy loop that never sleeps between
iterations is one turn, and AppKit's dispatch of an event between run-loop runs falls inside it.
`src/main.rs`'s `supervise` wraps each actor in `turns::timed`, which times each of its polls (two
`Instant` reads, about 24 a wake with twelve actors joined, since `tokio::join!` polls every
unfinished branch) and sums them by name. Every place rini changes a window on the main thread calls
`turns::changed`, into a ring of the last 64 per thread.

A turn over 8ms, one frame at 120Hz, becomes a `MainTurn`: when it woke, how long it ran, each actor's
polls, the busy time no poll covers (AppKit, Core Animation's commit and flush, event dispatch), and
the window changes from 50ms before it began to its end, since the flush that holds a turn commits
changes earlier turns made. The handler `src/main.rs` gives sends it to the flight engine, which
journals it (`main_turn` in `src/transitions/docs/journal.md`). A quick turn allocates nothing; a
slow one, two `Vec`s and a channel send.
