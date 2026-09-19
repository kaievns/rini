//! One unbounded channel per thread-owning loop. The current tracing span rides along with each
//! message so a handler's spans nest under the sender's. See `docs/run-loop-executor.md`.

use tokio::sync::mpsc::error::SendError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::Span;

pub struct Sender<Event>(UnboundedSender<(Span, Event)>);
pub type Receiver<Event> = UnboundedReceiver<(Span, Event)>;

pub fn channel<Event>() -> (Sender<Event>, Receiver<Event>) {
    let (tx, rx) = unbounded_channel();
    (Sender(tx), rx)
}

impl<Event> Sender<Event> {
    pub fn send(&self, event: Event) {
        // Most of the time we can ignore send errors, they just indicate the
        // app is shutting down.
        _ = self.try_send(event)
    }

    pub fn try_send(&self, event: Event) -> Result<(), SendError<(Span, Event)>> {
        self.0.send((Span::current(), event))
    }
}

impl<Event> Clone for Sender<Event> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<Event> std::fmt::Debug for Sender<Event> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("channel::Sender(...)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_is_lossy_once_the_receiver_is_gone_and_try_send_reports_it() {
        let (tx, rx) = channel::<u8>();
        drop(rx);
        tx.send(1);
        assert!(tx.try_send(2).is_err());
    }

    #[test]
    fn messages_arrive_in_order_with_a_span_attached() {
        let (tx, mut rx) = channel::<u8>();
        tx.send(1);
        tx.clone().send(2);
        let (_span, first) = rx.try_recv().unwrap();
        let (_span, second) = rx.try_recv().unwrap();
        assert_eq!((first, second), (1, 2));
        assert!(rx.try_recv().is_err());
    }
}
