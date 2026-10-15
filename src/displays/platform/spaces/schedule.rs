//! When the spaces actor hears from itself again: a refresh or a churn's check, after a delay.
use dispatchr::queue;
use dispatchr::time::Time;
use rini_runloop::dispatch::DispatchExt;

use super::{Notification, Sender};

/// Hands `notification` back to the actor `delay_ns` from now.
pub trait Schedule {
    fn after(&self, delay_ns: i64, notification: Notification);
}

/// The main queue, which production schedules on.
pub struct MainQueue(pub Sender);

impl Schedule for MainQueue {
    fn after(&self, delay_ns: i64, notification: Notification) {
        queue::main().after_f_s(
            Time::new_after(Time::NOW, delay_ns),
            (self.0.clone(), notification),
            |(sender, notification)| sender.send(notification),
        );
    }
}

/// What a test schedules: every notification asked for, in order, with its delay, and none sent.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct Recorded(std::rc::Rc<std::cell::RefCell<Vec<(i64, Notification)>>>);

#[cfg(test)]
impl Recorded {
    /// Everything scheduled since the last `take`.
    pub fn take(&self) -> Vec<(i64, Notification)> { self.0.take() }
}

#[cfg(test)]
impl Schedule for Recorded {
    fn after(&self, delay_ns: i64, notification: Notification) {
        self.0.borrow_mut().push((delay_ns, notification));
    }
}
