//! The span-carrying channel every actor in the application is wired with. Declared once here so
//! the wiring in `main.rs` and each actor agree on the type.

pub use rini_runloop::channel::{Receiver, Sender, channel};
