//! Carrying out the reactor's `Request`s: frame writes, a workspace switch's positions, closes,
//! refreshes, and handing raises to their own task.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{WindowId, WindowServerId};
use tracing::{instrument, warn};

use super::raise::RaiseRequest;
use super::{State, trace};
use crate::windows::domain::frame_write::{FrameWrite, frame_write};
use crate::windows::domain::request::Request;
use crate::windows::domain::transaction::{Requested, TransactionId};
use crate::windows::event::Event;
use crate::windows::platform::ax::element::Error as AxError;
use crate::windows::platform::ax::world::AxWorld;

impl<W: AxWorld> State<W> {
    /// Whether the thread is done: true once the reactor has asked it to end.
    #[instrument(skip_all, fields(pid = ?self.pid, ?request))]
    pub(super) fn handle_request(&mut self, request: Request) -> Result<bool, AxError> {
        match request {
            Request::Terminate => {
                self.send_event(Event::ApplicationThreadTerminated(self.pid));
                return Ok(true);
            }
            Request::WindowMaybeDestroyed(wid) => self.check_window_still_exists(wid)?,
            Request::CloseWindow(window_server_id) => self.close_window(window_server_id),
            Request::GetVisibleWindows => self.refresh_visible_windows()?,
            Request::ApplicationGloballyActivated(pid) => {
                if pid == self.pid {
                    self.on_global_activation()?;
                }
            }
            Request::SetWindowFrame { window, frame, txid, .. } => {
                self.set_frame(window, frame, txid)?
            }
            Request::SetBatchWindowFrame(frames, txid, _) => {
                for (wid, desired) in frames {
                    self.set_frame(wid, desired, txid)?;
                }
            }
            Request::SetWorkspaceSwitchPositions(positions, txid, _) => {
                for (wid, position) in positions {
                    self.set_switch_position(wid, position, txid)?;
                }
            }
            Request::Raise {
                windows,
                token,
                sequence_id,
                quiet,
            } => {
                self.raises_tx.send(RaiseRequest {
                    windows,
                    token,
                    sequence_id,
                    quiet,
                });
            }
        }
        Ok(false)
    }

    fn check_window_still_exists(&mut self, wid: WindowId) -> Result<(), AxError> {
        if wid.pid != self.pid {
            return Ok(());
        }

        // If we don't know this window, nothing to verify.
        if !self.windows.contains_key(&wid) {
            return Ok(());
        }

        // Trigger a visible windows refresh. If the window is gone, the reactor
        // will detect it via missing membership and tear down state.
        self.refresh_visible_windows()
    }

    fn close_window(&self, window_server_id: Option<WindowServerId>) {
        if let Some(wsid) = window_server_id
            && let Err(err) = self.ax.server().make_key_window(self.pid, wsid)
        {
            warn!(pid = self.pid, ?wsid, ?err, "Failed to focus close target");
            return;
        }
        if !self.ax.post_command_w() {
            warn!(pid = self.pid, ?window_server_id, "Failed to post Command-W");
        }
    }

    fn set_frame(
        &mut self,
        wid: WindowId,
        desired: CGRect,
        txid: TransactionId,
    ) -> Result<(), AxError> {
        let Some((elem, server_id)) = self.write_target(wid, txid) else {
            return Ok(());
        };
        let Some(frame) = self.write_frame(wid, &elem, server_id, desired)? else {
            return Ok(());
        };
        self.send_event(Event::WindowFrameChanged {
            window: wid,
            frame,
            last_seen: Some(txid),
            requested: Requested(true),
            mouse_state: None,
        });
        Ok(())
    }

    fn set_switch_position(
        &mut self,
        wid: WindowId,
        position: CGPoint,
        txid: TransactionId,
    ) -> Result<(), AxError> {
        let Some((elem, _)) = self.write_target(wid, txid) else {
            return Ok(());
        };

        let _ = self.ax.set_position(&elem, position);

        // Preserve the existing per-window acknowledgement semantics. In
        // particular, report the frame AX actually accepted rather than the
        // requested position combined with a cached size.
        let Some(frame) =
            self.handle_ax_result(wid, trace("frame", wid, || self.ax.frame(&elem)))?
        else {
            return Ok(());
        };
        self.send_event(Event::WindowFrameChanged {
            window: wid,
            frame,
            last_seen: Some(txid),
            requested: Requested(true),
            mouse_state: None,
        });
        Ok(())
    }

    /// Puts the window at `desired` and answers the frame read back after the last write.
    fn write_frame(
        &mut self,
        wid: WindowId,
        elem: &W::Element,
        server_id: Option<WindowServerId>,
        desired: CGRect,
    ) -> Result<Option<CGRect>, AxError> {
        let current = server_id
            .and_then(|id| self.ax.server().server_frame(id))
            .map(|frame| frame.size);
        if frame_write(current, desired.size) == FrameWrite::PositionOnly {
            let _ = self.ax.set_position(elem, desired.origin);
            let Some(landed) =
                self.handle_ax_result(wid, trace("frame", wid, || self.ax.frame(elem)))?
            else {
                return Ok(None);
            };
            if frame_write(Some(landed.size), desired.size) == FrameWrite::PositionOnly {
                return Ok(Some(landed));
            }
        }
        let _ = self.ax.set_size(elem, desired.size);
        let _ = self.ax.set_position(elem, desired.origin);
        let _ = self.ax.set_size(elem, desired.size);
        self.handle_ax_result(wid, trace("frame", wid, || self.ax.frame(elem)))
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_graphics::CGError;

    use super::super::fixtures::{PID, announced, app_with, peer, raisable, rect, state_with};
    use super::*;
    use crate::windows::domain::ax_events::AxNotificationKind;
    use crate::windows::domain::request::SuppressEnhancedUi;
    use crate::windows::platform::ax::world::FakeAx;

    /// A frame write that changes the size is size, then position, then size AGAIN.
    ///
    /// Not a typo and not belt-and-braces. AppKit clamps a size against the window's CURRENT position
    /// — a window near the right edge of a screen cannot grow until it has moved — so sizing before
    /// the move can be refused, and sizing after it is what actually lands. The first size is what
    /// lets the move succeed for a window that has to grow and shift at once.
    ///
    /// Nothing pinned this order before: the file had no tests, and the three lines look redundant
    /// enough that a tidy-up would drop one.
    #[test]
    fn a_resize_sizes_then_moves_then_sizes_again() {
        let window = 7;
        let (mut state, _sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 100., 100.)));
        state
            .ax
            .server
            .set_server_frame(WindowServerId::new(window), Some(rect(0., 0., 100., 100.)));
        let wid = state
            .register_window(window, peer(window, rect(0., 0., 100., 100.)))
            .expect("the window registers")
            .1;
        state.ax.positions.borrow_mut().clear();
        state.ax.sizes.borrow_mut().clear();

        let desired = rect(500., 300., 800., 600.);
        state
            .handle_request(Request::SetWindowFrame {
                window: wid,
                frame: desired,
                txid: TransactionId::default(),
                enhanced_ui: SuppressEnhancedUi::No,
            })
            .expect("the write succeeds");

        assert_eq!(
            state.ax.sizes.borrow().as_slice(),
            &[(window, desired.size), (window, desired.size)],
            "sized twice, before and after the move"
        );
        assert_eq!(
            state.ax.positions.borrow().as_slice(),
            &[(window, desired.origin)],
            "moved once, between the two sizings"
        );
    }

    /// A move that keeps the window's size writes the position alone. See "Writing a frame" in
    /// `specs/windows.md`.
    #[test]
    fn a_same_size_move_writes_the_position_alone() {
        let window = 7;
        let (mut state, _sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 800., 600.)));
        state
            .ax
            .server
            .set_server_frame(WindowServerId::new(window), Some(rect(0., 0., 800., 600.)));
        let wid = state
            .register_window(window, peer(window, rect(0., 0., 800., 600.)))
            .expect("the window registers")
            .1;
        state.ax.positions.borrow_mut().clear();
        state.ax.sizes.borrow_mut().clear();

        let desired = rect(500., 300., 800., 600.);
        state
            .handle_request(Request::SetWindowFrame {
                window: wid,
                frame: desired,
                txid: TransactionId::default(),
                enhanced_ui: SuppressEnhancedUi::No,
            })
            .expect("the write succeeds");

        assert!(state.ax.sizes.borrow().is_empty(), "never sized");
        assert_eq!(
            state.ax.positions.borrow().as_slice(),
            &[(window, desired.origin)],
            "moved once"
        );
    }

    /// A same-size move whose read-back size is off is written again in full, and the reactor hears
    /// the frame read after that, not the one the move left.
    #[test]
    fn a_same_size_move_that_lands_at_another_size_is_written_in_full() {
        let window = 7;
        let (mut state, sink) =
            state_with(FakeAx::with_one_window(window, rect(0., 0., 800., 600.)));
        state
            .ax
            .server
            .set_server_frame(WindowServerId::new(window), Some(rect(0., 0., 800., 600.)));
        let wid = state
            .register_window(window, peer(window, rect(0., 0., 800., 600.)))
            .expect("the window registers")
            .1;
        state.ax.positions.borrow_mut().clear();
        state.ax.sizes.borrow_mut().clear();
        sink.0.borrow_mut().clear();

        let desired = rect(2000., 0., 800., 600.);
        let clamped = rect(2000., 0., 640., 600.);
        state.ax.frame_reads.borrow_mut().insert(window, [clamped, desired].into());
        state
            .handle_request(Request::SetWindowFrame {
                window: wid,
                frame: desired,
                txid: TransactionId::default(),
                enhanced_ui: SuppressEnhancedUi::No,
            })
            .expect("the write succeeds");

        assert_eq!(
            state.ax.positions.borrow().as_slice(),
            &[(window, desired.origin), (window, desired.origin)],
            "moved alone, then moved again between the two sizings"
        );
        assert_eq!(
            state.ax.sizes.borrow().as_slice(),
            &[(window, desired.size), (window, desired.size)],
            "sized twice once the move came back at another size"
        );
        let announced = sink.0.borrow();
        assert!(
            matches!(
                announced.as_slice(),
                [Event::WindowFrameChanged { window: w, frame, .. }] if *w == wid && *frame == desired
            ),
            "{announced:?}"
        );
    }

    /// A batch decides each window on its own size: the one the window server has at the desired
    /// size moves alone, the one whose size cannot be read is sized around its move.
    #[test]
    fn a_batch_writes_each_window_by_its_own_size() {
        let (kept, unread) = (7, 8);
        let mut ax = FakeAx::with_one_window(kept, rect(0., 0., 800., 600.));
        ax.windows.push(unread);
        ax.describe_standard_window(unread, rect(800., 0., 400., 600.));
        ax.server
            .set_server_frame(WindowServerId::new(kept), Some(rect(0., 0., 800., 600.)));
        let (mut state, _sink) = state_with(ax);
        let kept_wid = state
            .register_window(kept, peer(kept, rect(0., 0., 800., 600.)))
            .expect("registers")
            .1;
        let unread_wid = state
            .register_window(unread, peer(unread, rect(800., 0., 400., 600.)))
            .expect("registers")
            .1;
        state.ax.positions.borrow_mut().clear();
        state.ax.sizes.borrow_mut().clear();

        let (to_kept, to_unread) = (rect(400., 0., 800., 600.), rect(1200., 0., 400., 600.));
        state
            .handle_request(Request::SetBatchWindowFrame(
                vec![(kept_wid, to_kept), (unread_wid, to_unread)],
                TransactionId::default(),
                SuppressEnhancedUi::No,
            ))
            .expect("the writes succeed");

        assert_eq!(
            state.ax.sizes.borrow().as_slice(),
            &[(unread, to_unread.size), (unread, to_unread.size)],
            "only the window of unknown size is sized"
        );
        assert_eq!(
            state.ax.positions.borrow().as_slice(),
            &[(kept, to_kept.origin), (unread, to_unread.origin)],
            "both move, in the batch's order"
        );
    }

    /// A batch skips a window the thread does not track, another application's included, and
    /// writes and reports the rest.
    #[test]
    fn a_batch_skips_a_window_it_does_not_track_and_writes_the_rest() {
        let (mut state, sink, wids) = app_with(&[7]);
        let (untracked, foreign) = (WindowId::new(PID, 9), WindowId::new(PID + 1, 7));
        let to = rect(400., 0., 100., 100.);

        let frames = vec![(untracked, to), (foreign, to), (wids[0], to)];
        let done = state.handle_request(Request::SetBatchWindowFrame(
            frames,
            TransactionId::default(),
            SuppressEnhancedUi::No,
        ));

        assert!(matches!(done, Ok(false)));
        assert_eq!(state.ax.positions.borrow().as_slice(), &[(7, to.origin)]);
        let events = announced(&sink);
        assert!(
            matches!(events.as_slice(), [Event::WindowFrameChanged { window: w, .. }] if *w == wids[0]),
            "{events:?}"
        );
    }

    /// A workspace switch moves each window and reports the frame read back, under the switch's
    /// txid, which the window's next move report carries too.
    #[test]
    fn a_switch_position_is_reported_as_read_back_under_its_txid() {
        let (mut state, sink, wids) = app_with(&[7]);
        let txid = TransactionId::default().next();
        let landed = rect(640., 0., 100., 100.);
        state.ax.frame_reads.borrow_mut().insert(7, [landed].into());

        state
            .handle_request(Request::SetWorkspaceSwitchPositions(
                vec![(wids[0], CGPoint::new(600., 0.))],
                txid,
                SuppressEnhancedUi::No,
            ))
            .expect("handled");
        state.handle_notification(7, AxNotificationKind::WindowMoved, Some(wids[0]));

        assert_eq!(state.ax.positions.borrow().as_slice(), &[(
            7,
            CGPoint::new(600., 0.)
        )]);
        let events = announced(&sink);
        assert!(
            matches!(
                events.as_slice(),
                [
                    Event::WindowFrameChanged { window: a, frame, last_seen: Some(t), requested: Requested(true), mouse_state: None },
                    Event::WindowFrameChanged { window: b, last_seen: Some(u), requested: Requested(false), .. },
                ] if *a == wids[0] && *frame == landed && *t == txid && *b == wids[0] && *u == txid
            ),
            "{events:?}"
        );
    }

    /// A close makes the window key through the window server, then posts Command-W, so the
    /// application closes that window its own way.
    #[test]
    fn a_close_makes_the_window_key_then_posts_command_w() {
        let (mut state, _sink, _) = raisable(7);

        state
            .handle_request(Request::CloseWindow(Some(WindowServerId::new(7))))
            .expect("handled");

        assert_eq!(state.ax.made_key(), [(PID, WindowServerId::new(7))]);
        assert_eq!(state.ax.command_w_posts.get(), 1, "Command-W posted once");
    }

    /// Command-W closes whichever window is key, so a close whose window could not be made key
    /// posts none.
    #[test]
    fn a_close_whose_window_cannot_be_made_key_posts_no_command_w() {
        let (mut state, _sink, _) = raisable(7);
        state.ax.server.fail_make_key_window(Some(CGError::Failure));

        state
            .handle_request(Request::CloseWindow(Some(WindowServerId::new(7))))
            .expect("handled");

        assert_eq!(state.ax.made_key(), [(PID, WindowServerId::new(7))], "asked");
        assert_eq!(state.ax.command_w_posts.get(), 0, "and nothing posted");
    }

    #[test]
    fn a_close_of_a_window_with_no_server_id_posts_command_w_alone() {
        let (mut state, _sink, _) = raisable(7);

        state.handle_request(Request::CloseWindow(None)).expect("handled");

        assert!(state.ax.made_key().is_empty(), "nothing to make key");
        assert_eq!(state.ax.command_w_posts.get(), 1);
    }
}
