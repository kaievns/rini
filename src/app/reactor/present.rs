//! What a layout pass is allowed to touch when it sends frames to the applications: two borrows,
//! one to record where a window is going, one to mint the transaction id that lets the frame report
//! coming back be matched to the write that caused it, so nothing here needs a reactor to be
//! exercised. `src/transitions/docs/coordination.md` calls this the
//! `present(motion)` boundary under "Structural findings". `instant_requests` is what a pass that
//! does not fly sends one application.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{WindowId, WindowServerId};

use crate::windows::domain::request::{Request, SuppressEnhancedUi};
use crate::windows::domain::transaction::{TransactionId, WindowTxStore};
use crate::workspaces::WindowStore;

/// The frame-writing capability: record a destination, and number the write.
pub(crate) struct Present<'a> {
    windows: &'a mut WindowStore,
    /// `&` rather than `&mut` because the transaction store is interior-mutable. It is shared with
    /// the app threads, which report against it.
    transactions: &'a WindowTxStore,
}

impl<'a> Present<'a> {
    pub(crate) fn new(windows: &'a mut WindowStore, transactions: &'a WindowTxStore) -> Self {
        Self { windows, transactions }
    }

    pub(crate) fn server_id(&self, window: WindowId) -> Option<WindowServerId> {
        self.windows.window(window).and_then(|state| state.info.sys_id)
    }

    /// Record one window's destination and number the write.
    ///
    /// The frame goes into the store before the request goes out, so a report arriving while the
    /// write is in flight is compared against where the window is GOING rather than where it was.
    pub(crate) fn commit(&mut self, window: WindowId, to: CGRect) -> TransactionId {
        let txid = match self.server_id(window) {
            Some(wsid) => {
                let txid = self.transactions.next_txid(wsid);
                self.transactions.insert(wsid, txid, to);
                txid
            }
            None => TransactionId::default(),
        };
        if let Some(state) = self.windows.window_mut(window) {
            state.frame_monotonic = to;
        }
        txid
    }

    /// Record a batch of frames for ONE application under a single transaction id.
    ///
    /// One id for the batch, not one per window. The application applies them together, so a
    /// per-window id would have each report matched against a different write and the batch would
    /// read as partly stale. The first window with a window-server id mints it; every other window
    /// in the batch is told to expect the same one.
    ///
    /// Windows with no server id still get their frame recorded — rini knows where they are going
    /// even when it cannot match the report.
    pub(crate) fn commit_app_batch(&mut self, frames: &[(WindowId, CGRect)]) -> TransactionId {
        let mut txid = TransactionId::default();
        let mut entries: Vec<(WindowServerId, TransactionId, CGRect)> = Vec::new();
        for &(window, to) in frames {
            if let Some(wsid) = self.server_id(window) {
                if entries.is_empty() {
                    txid = self.transactions.next_txid(wsid);
                } else {
                    self.transactions.set_last_txid(wsid, txid);
                }
                entries.push((wsid, txid, to));
            }
        }
        for (wsid, txid, to) in entries {
            self.transactions.insert(wsid, txid, to);
        }
        for &(window, to) in frames {
            if let Some(state) = self.windows.window_mut(window) {
                state.frame_monotonic = to;
            }
        }
        txid
    }
}

/// The requests that place one app's share of an instant pass. Position-only passes (a workspace
/// switch) send origins for the windows whose size is unchanged, so the app skips the resize.
pub(crate) fn instant_requests(
    frames: Vec<(WindowId, CGRect, bool)>,
    txid: TransactionId,
    position_only: bool,
) -> Vec<Request> {
    if !position_only {
        return vec![Request::SetBatchWindowFrame(
            frames.into_iter().map(|(wid, frame, _)| (wid, frame)).collect(),
            txid,
            SuppressEnhancedUi::Yes,
        )];
    }
    let mut positions: Vec<(WindowId, CGPoint)> = Vec::new();
    let mut full_frames: Vec<(WindowId, CGRect)> = Vec::new();
    for (wid, frame, size_unchanged) in frames {
        if size_unchanged {
            positions.push((wid, frame.origin));
        } else {
            full_frames.push((wid, frame));
        }
    }
    let mut requests = Vec::with_capacity(2);
    if !positions.is_empty() {
        requests.push(Request::SetWorkspaceSwitchPositions(
            positions,
            txid,
            SuppressEnhancedUi::Yes,
        ));
    }
    if !full_frames.is_empty() {
        requests.push(Request::SetBatchWindowFrame(
            full_frames,
            txid,
            SuppressEnhancedUi::Yes,
        ));
    }
    requests
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;
    use crate::windows::domain::info::WindowInfo;
    use crate::windows::domain::state::WindowState;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn frame(x: f64) -> CGRect { CGRect::new(CGPoint::new(x, 0.0), CGSize::new(100.0, 100.0)) }

    fn store_with(windows: &[(WindowId, Option<WindowServerId>)]) -> WindowStore {
        let mut store = WindowStore::default();
        for &(window, sys_id) in windows {
            store.insert_window(window, WindowState {
                info: WindowInfo {
                    is_standard: true,
                    is_root: true,
                    is_minimized: false,
                    is_resizable: true,
                    min_size: None,
                    max_size: None,
                    title: String::new(),
                    frame: frame(0.0),
                    sys_id,
                    bundle_id: None,
                    path: None,
                    ax_role: None,
                    ax_subrole: None,
                    is_modal: false,
                },
                frame_monotonic: frame(0.0),
                is_manageable: true,
                ignore_app_rule: false,
            });
        }
        store
    }

    #[test]
    fn a_commit_records_the_destination_before_the_request_goes_out() {
        let window = WindowId::new(10, 1);
        let mut store = store_with(&[(window, Some(WindowServerId::new(1)))]);
        let transactions = WindowTxStore::new();
        let mut present = Present::new(&mut store, &transactions);

        let txid = present.commit(window, frame(500.0));

        assert_eq!(store.window(window).unwrap().frame_monotonic, frame(500.0));
        assert_eq!(transactions.target(&WindowServerId::new(1)), Some(frame(500.0)));
        assert_eq!(transactions.last_txid(&WindowServerId::new(1)), txid);
    }

    /// A window with no window-server id cannot have its report matched, but rini still knows where
    /// it is going. Dropping the frame would make the next pass think it had not moved.
    #[test]
    fn a_window_with_no_server_id_still_gets_its_frame_recorded() {
        let window = WindowId::new(10, 1);
        let mut store = store_with(&[(window, None)]);
        let transactions = WindowTxStore::new();
        let mut present = Present::new(&mut store, &transactions);

        let txid = present.commit(window, frame(500.0));

        assert_eq!(txid, TransactionId::default(), "nothing to number");
        assert_eq!(store.window(window).unwrap().frame_monotonic, frame(500.0));
    }

    /// The rule the batching exists for: one id for the whole batch, because the application applies
    /// them together. A per-window id would have each report matched against a different write.
    #[test]
    fn every_window_in_an_app_batch_shares_one_transaction_id() {
        let windows = [
            WindowId::new(10, 1),
            WindowId::new(10, 2),
            WindowId::new(10, 3),
        ];
        let ids = [
            WindowServerId::new(1),
            WindowServerId::new(2),
            WindowServerId::new(3),
        ];
        let mut store = store_with(&[
            (windows[0], Some(ids[0])),
            (windows[1], Some(ids[1])),
            (windows[2], Some(ids[2])),
        ]);
        let transactions = WindowTxStore::new();
        let mut present = Present::new(&mut store, &transactions);

        let txid = present.commit_app_batch(&[
            (windows[0], frame(0.0)),
            (windows[1], frame(100.0)),
            (windows[2], frame(200.0)),
        ]);

        for id in ids {
            assert_eq!(transactions.last_txid(&id), txid, "{id:?} is out of the batch");
        }
        assert_eq!(transactions.target(&ids[1]), Some(frame(100.0)));
        assert_eq!(transactions.target(&ids[2]), Some(frame(200.0)));
    }

    #[test]
    fn a_batch_records_every_frame_even_for_windows_without_a_server_id() {
        let windows = [WindowId::new(10, 1), WindowId::new(10, 2)];
        let mut store = store_with(&[
            (windows[0], None),
            (windows[1], Some(WindowServerId::new(2))),
        ]);
        let transactions = WindowTxStore::new();
        let mut present = Present::new(&mut store, &transactions);

        let txid =
            present.commit_app_batch(&[(windows[0], frame(10.0)), (windows[1], frame(20.0))]);

        assert_eq!(store.window(windows[0]).unwrap().frame_monotonic, frame(10.0));
        assert_eq!(store.window(windows[1]).unwrap().frame_monotonic, frame(20.0));
        assert_eq!(
            transactions.last_txid(&WindowServerId::new(2)),
            txid,
            "the first window with an id mints it, not the first window"
        );
    }

    #[test]
    fn an_empty_batch_numbers_nothing() {
        let mut store = WindowStore::default();
        let transactions = WindowTxStore::new();
        let mut present = Present::new(&mut store, &transactions);
        assert_eq!(present.commit_app_batch(&[]), TransactionId::default());
    }

    #[test]
    fn position_only_requests_split_moves_from_resizes() {
        let txid = TransactionId::default();
        let w = |i| WindowId::new(1, i);
        let frames = vec![
            (w(1), rect(0.0, 0.0, 10.0, 10.0), true),
            (w(2), rect(5.0, 0.0, 20.0, 10.0), false),
        ];
        let requests = instant_requests(frames.clone(), txid, true);
        assert!(
            matches!(&requests[0], Request::SetWorkspaceSwitchPositions(p, _, SuppressEnhancedUi::Yes) if p == &vec![(w(1), CGPoint::new(0.0, 0.0))])
        );
        assert!(
            matches!(&requests[1], Request::SetBatchWindowFrame(f, _, SuppressEnhancedUi::Yes) if f.len() == 1 && f[0].0 == w(2))
        );
        let full = instant_requests(frames, txid, false);
        assert_eq!(full.len(), 1);
        assert!(
            matches!(&full[0], Request::SetBatchWindowFrame(f, _, SuppressEnhancedUi::Yes) if f.len() == 2)
        );
    }
}
