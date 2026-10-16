# `rini-runloop` — the CFRunLoop executor, timers, span channels

Every thread in rini that owns a `CFRunLoop` drives one future from it. This crate is
that machinery, and nothing else: no window, no workspace, no layout.

## What it owns

| | |
|---|---|
| `executor.rs` | `Executor::run` / `run_main`: one future per run loop, polled from a manual `CFRunLoopSource`; `run_interactive`, `run` at the main thread's quality of service; `sleep`, the run-loop-backed replacement for `tokio::time::sleep` |
| `run_loop.rs` | `WakeupHandle`, the manual source the executor polls from; `RepeatingTimer`, which `sleep` waits on; `TurnObserver`, the observer pair that opens a turn as the loop wakes and closes it as it goes back to sleep |
| `turns.rs` | a turn's time, folded: each actor's polls (`timed`), the time outside them, and the window changes noted (`changed`) from 50ms before it; a turn over 8ms is a `MainTurn` |
| `timer.rs` | `Timer`: an awaitable `CFRunLoopTimer` |
| `channel.rs` | An unbounded mpsc that carries the sender's tracing span with each message |
| `dispatch.rs` | `DispatchExt::after_f`/`after_f_s` (run work on a dispatch queue after a delay; `after_f_s` takes a `Send` context, since the work runs on the queue's thread) and `block_on` |

## The shape that matters

**`tokio::time` panics here** and took the whole window manager down twice. There is no
tokio reactor, only a `CFRunLoop`. Use `executor::sleep` or `RepeatingTimer`.

**Channels are unbounded on purpose.** A bounded channel between threads that also
service run loops would deadlock the main thread on backpressure.

**Spans nest across threads.** A handler's spans appear under the sender's, which is why
a log line from an app thread can be traced back to the keystroke that caused it.

## Detail

- [`run-loop-executor.md`](run-loop-executor.md) — why not a tokio runtime, what
  `tokio::time` costs, main-task lifetime, the channel, and timing the main thread's turns
