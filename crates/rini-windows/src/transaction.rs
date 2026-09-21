//! Frame-write bookkeeping. Every frame the reactor asks for carries a `TransactionId`; the app
//! actor stamps the AX echo with it, so a `WindowFrameChanged` can be told from a user move
//! (`Requested`). `WindowTxStore` is the per-window-server-id table both sides read.
use serde::{Deserialize, Serialize};

#[derive(Default, Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransactionId(u32);

/// Whether a frame change is the echo of a frame rini asked for, or the user's doing.
#[derive(Serialize, Deserialize, Debug)]
pub struct Requested(pub bool);

impl TransactionId {
    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Manages window transaction IDs and their associated target frames.

use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use objc2_core_foundation::CGRect;

use rini_core::ids::WindowServerId;

#[derive(Clone, Copy, Debug, Default)]
pub struct TxRecord {
    pub txid: TransactionId,
    pub target: Option<CGRect>,
}

/// Thread-safe cache mapping window server IDs to their last known transaction.
#[derive(Clone, Default, Debug)]
pub struct WindowTxStore(Arc<DashMap<WindowServerId, TxRecord>>);

impl WindowTxStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, id: WindowServerId, txid: TransactionId, target: CGRect) {
        match self.0.entry(id) {
            Entry::Occupied(mut entry) => {
                *entry.get_mut() = TxRecord { txid, target: Some(target) }
            }
            Entry::Vacant(entry) => {
                entry.insert(TxRecord { txid, target: Some(target) });
            }
        }
    }

    pub fn get(&self, id: &WindowServerId) -> Option<TxRecord> {
        self.0.get(id).map(|entry| *entry)
    }

    pub fn remove(&self, id: &WindowServerId) {
        self.0.remove(id);
    }

    pub fn clear_target(&self, id: &WindowServerId) {
        if let Some(mut record) = self.0.get_mut(id) {
            record.target = None;
        }
    }

    pub fn next_txid(&self, id: WindowServerId) -> TransactionId {
        let new_txid = match self.0.entry(id) {
            Entry::Occupied(mut entry) => {
                let record = entry.get_mut();
                let new_txid = record.txid.next();
                *record = TxRecord { txid: new_txid, target: None };
                new_txid
            }
            Entry::Vacant(entry) => {
                let txid = TransactionId::default().next();
                entry.insert(TxRecord { txid, target: None });
                txid
            }
        };
        new_txid
    }

    pub fn set_last_txid(&self, id: WindowServerId, txid: TransactionId) {
        match self.0.entry(id) {
            Entry::Occupied(mut entry) => {
                let record = entry.get_mut();
                record.txid = txid;
                record.target = None;
            }
            Entry::Vacant(entry) => {
                entry.insert(TxRecord { txid, target: None });
            }
        }
    }

    pub fn last_txid(&self, id: &WindowServerId) -> TransactionId {
        self.get(id).map(|record| record.txid).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

    use super::*;

    #[test]
    fn clear_target_keeps_last_txid() {
        let store = WindowTxStore::new();
        let wsid = WindowServerId::new(1);
        let txid = store.next_txid(wsid);
        let target = CGRect::new(CGPoint::new(10.0, 20.0), CGSize::new(30.0, 40.0));
        store.insert(wsid, txid, target);

        store.clear_target(&wsid);

        let record = store.get(&wsid).expect("tx record should exist");
        assert_eq!(record.txid, txid);
        assert_eq!(record.target, None);
    }

    #[test]
    fn next_txid_advances_after_clearing_target() {
        let store = WindowTxStore::new();
        let wsid = WindowServerId::new(2);

        let txid_1 = store.next_txid(wsid);
        let target = CGRect::new(CGPoint::new(1.0, 2.0), CGSize::new(3.0, 4.0));
        store.insert(wsid, txid_1, target);
        store.clear_target(&wsid);

        let txid_2 = store.next_txid(wsid);
        assert_eq!(txid_2, txid_1.next());
    }

    #[test]
    fn set_last_txid_clears_any_stale_target() {
        let store = WindowTxStore::new();
        let wsid = WindowServerId::new(3);
        let txid_1 = store.next_txid(wsid);
        let target = CGRect::new(CGPoint::new(5.0, 6.0), CGSize::new(7.0, 8.0));
        store.insert(wsid, txid_1, target);

        let txid_2 = txid_1.next();
        store.set_last_txid(wsid, txid_2);

        let record = store.get(&wsid).expect("tx record should exist");
        assert_eq!(record.txid, txid_2);
        assert_eq!(record.target, None);
    }
}


/// A per-window counter that tracks the last time the reactor sent a request to
/// change the window frame.
#[derive(Debug)]
pub struct TransactionManager {
    pub store: WindowTxStore,
}

impl TransactionManager {
    pub fn new(store: WindowTxStore) -> Self {
        Self { store }
    }

    /// Stores a transaction ID for a window with its target frame.
    pub fn store_txid(&self, wsid: WindowServerId, txid: TransactionId, target: CGRect) {
        self.store.insert(wsid, txid, target);
    }

    /// Updates multiple transaction ID entries.
    pub fn update_txid_entries<I>(&self, entries: I)
    where
        I: IntoIterator<Item = (WindowServerId, TransactionId, CGRect)>,
    {
        for (wsid, txid, target) in entries {
            self.store.insert(wsid, txid, target);
        }
    }

    /// Removes the transaction ID entry for a window.
    pub fn remove_for_window(&self, wsid: WindowServerId) {
        self.store.remove(&wsid);
    }

    /// Clears the pending target for a window while preserving its last txid.
    pub fn clear_target_for_window(&self, wsid: WindowServerId) {
        self.store.clear_target(&wsid);
    }

    /// Generates the next transaction ID for a window.
    pub fn generate_next_txid(&self, wsid: WindowServerId) -> TransactionId {
        self.store.next_txid(wsid)
    }

    /// Sets the last sent transaction ID for a window.
    pub fn set_last_sent_txid(&self, wsid: WindowServerId, txid: TransactionId) {
        self.store.set_last_txid(wsid, txid);
    }

    /// Gets the last sent transaction ID for a window.
    pub fn get_last_sent_txid(&self, wsid: WindowServerId) -> TransactionId {
        self.store.last_txid(&wsid)
    }

    /// Gets the target frame for a window's transaction, if it exists.
    pub fn get_target_frame(&self, wsid: WindowServerId) -> Option<CGRect> {
        self.store.get(&wsid)?.target
    }
}
