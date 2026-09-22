# `rini-runloop` — the CFRunLoop executor, timers, span channels

Every thread in rini that owns a `CFRunLoop` drives one future from it. This crate is
that machinery, and nothing else: no window, no workspace, no layout.

## What it owns

| | |
|---|---|
| `executor.rs` | `Executor::run` / `run_main`: one future per run loop, polled from a manual `CFRunLoopSource` |
| `run_loop.rs` | `RepeatingTimer`, and `sleep` — the run-loop-backed replacements for `tokio::time` |
| `timer.rs` | `Timer`: an awaitable `CFRunLoopTimer` |
| `channel.rs` | An unbounded mpsc that carries the sender's tracing span with each message |
| `dispatch.rs` | The `libdispatch` declarations the timers need |

## The shape that matters

**`tokio::time` panics here** and took the whole window manager down twice. There is no
tokio reactor, only a `CFRunLoop`. Use `run_loop::sleep` or `RepeatingTimer`.

**Channels are unbounded on purpose.** A bounded channel between threads that also
service run loops would deadlock the main thread on backpressure.

**Spans nest across threads.** A handler's spans appear under the sender's, which is why
a log line from an app thread can be traced back to the keystroke that caused it.

## Detail

- [`run-loop-executor.md`](run-loop-executor.md) — why not a tokio runtime, what
  `tokio::time` costs, main-task lifetime, and the channel
