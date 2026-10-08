//! The run loop every rini actor lives on: a CFRunLoop-driven executor, timers, and the
//! span-carrying channel between actors, and the timing of a run loop's turns. No domain types. See
//! `docs/run-loop-executor.md`.
pub mod channel;
pub mod dispatch;
pub mod executor;
pub mod run_loop;
pub mod timer;
pub mod turns;
