# The run-loop executor

Every thread that owns a `CFRunLoop` in rini drives one future from it:
`executor::Executor::run` (or `run_main`, which calls `NSApp.run()` instead of
`CFRunLoop::run`). The main thread joins nine such loops; the reactor, the
config actor, the input tap and each observed application have their own thread
and their own loop.

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
"there is no reactor running" when polled under this executor. That panic took
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

## Message channels

`rini_runloop::channel` is an unbounded tokio mpsc carrying the sender's tracing
span with each message, so a handler's spans nest under the sender's. Unbounded
is deliberate: a bounded channel between threads that also service run loops
would deadlock the main thread on backpressure.
