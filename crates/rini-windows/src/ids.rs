//! Window and process identity. `WindowId` is rini's own (pid + per-process index, valid for the
//! owning process's lifetime); `WindowServerId` is the window server's CGWindowID.

use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

pub use libc::pid_t;
use serde::{Deserialize, Serialize};

/// An identifier representing a window.
///
/// This identifier is only valid for the lifetime of the process that owns it.
/// It is not stable across restarts of the window manager.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct WindowId {
    pub pid: pid_t,
    pub idx: NonZeroU32,
}

impl serde::ser::Serialize for WindowId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("WindowId", 2)?;
        s.serialize_field("pid", &self.pid)?;
        s.serialize_field("idx", &self.idx.get())?;
        s.end()
    }
}

impl<'de> serde::de::Deserialize<'de> for WindowId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        struct WindowIdVisitor;
        impl<'de> serde::de::Visitor<'de> for WindowIdVisitor {
            type Value = WindowId;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a WindowId struct (with fields `pid` and `idx`), a tuple/seq (pid, idx), or a debug string like `WindowId { pid: 123, idx: 456 }`",
                )
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                WindowId::from_debug_string(v)
                    .ok_or_else(|| E::custom("invalid WindowId debug string"))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<WindowId, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let pid: pid_t = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;

                let idx_u32: u32 = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;

                let idx = std::num::NonZeroU32::new(idx_u32)
                    .ok_or_else(|| serde::de::Error::custom("idx must be non-zero"))?;
                Ok(WindowId { pid, idx })
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut pid: Option<pid_t> = None;
                let mut idx: Option<u32> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "pid" => {
                            pid = Some(map.next_value()?);
                        }
                        "idx" => {
                            idx = Some(map.next_value()?);
                        }
                        // ignore unknown fields to be forward compatible
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }

                let pid = pid.ok_or_else(|| serde::de::Error::missing_field("pid"))?;
                let idx_val = idx.ok_or_else(|| serde::de::Error::missing_field("idx"))?;
                let nz = std::num::NonZeroU32::new(idx_val)
                    .ok_or_else(|| serde::de::Error::custom("idx must be non-zero"))?;

                Ok(WindowId { pid, idx: nz })
            }
        }

        deserializer.deserialize_any(WindowIdVisitor)
    }
}

impl WindowId {
    pub fn new(pid: pid_t, idx: u32) -> WindowId {
        WindowId {
            pid,
            idx: NonZeroU32::new(idx).unwrap(),
        }
    }

    /// Parse a WindowId from its string representation (format: "WindowId { pid: 123, idx: 456 }")
    pub fn from_debug_string(s: &str) -> Option<WindowId> {
        if !s.starts_with("WindowId { pid: ") {
            return None;
        }

        let s = s.strip_prefix("WindowId { pid: ")?;
        let (pid_str, rest) = s.split_once(", idx: ")?;
        let idx_str = rest.strip_suffix(" }")?;

        let pid: pid_t = pid_str.parse().ok()?;
        let idx: u32 = idx_str.parse().ok()?;

        Some(WindowId {
            pid,
            idx: std::num::NonZeroU32::new(idx)?,
        })
    }

    pub fn to_debug_string(&self) -> String {
        format!("{:?}", self)
    }
}

impl From<WindowId> for rini_protocol::WindowId {
    fn from(value: WindowId) -> Self {
        Self {
            pid: value.pid,
            idx: value.idx.get(),
        }
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowServerId(pub u32);

impl WindowServerId {
    #[inline]
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    #[inline]
    pub fn as_u32(self) -> u32 {
        self.0
    }

    #[inline]
    pub fn as_nonzero(self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.0)
    }
}

impl From<WindowServerId> for u32 {
    #[inline]
    fn from(id: WindowServerId) -> Self {
        id.0
    }
}

impl From<WindowId> for WindowServerId {
    fn from(id: WindowId) -> Self {
        Self(id.idx.into())
    }
}


pub trait BTreeExt {
    fn remove_all_for_pid(&mut self, pid: pid_t) -> Self;
}

// There's not currently a stable way to remove only a range, so we have
// to do this split/extend dance. Is it faster than scanning through all
// the keys? Who knows!

impl BTreeExt for BTreeSet<WindowId> {
    fn remove_all_for_pid(&mut self, pid: pid_t) -> Self {
        let mut split = self.split_off(&PidRange(pid));
        self.extend(split.split_off(&PidRange(pid + 1)));
        split
    }
}

impl<V> BTreeExt for BTreeMap<WindowId, V> {
    fn remove_all_for_pid(&mut self, pid: pid_t) -> Self {
        let mut split = self.split_off(&PidRange(pid));
        self.extend(split.split_off(&PidRange(pid + 1)));
        split
    }
}

#[derive(Ord, PartialOrd, Eq, PartialEq)]
#[repr(transparent)]
struct PidRange(pid_t);

// Technically this violates the Borrow requirements by having Ord/Eq
// behave differently than the original type, but we are working around
// API limitations and it should not matter for a reasonable implementation
// of `split_off`.
impl Borrow<PidRange> for WindowId {
    fn borrow(&self) -> &PidRange {
        // Safety: PidRange is repr(transparent).
        unsafe { &*std::ptr::addr_of!(self.pid).cast() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wid() -> WindowId {
        WindowId::new(42, 7)
    }

    #[test]
    fn window_id_json_round_trips_as_a_struct() {
        let json = serde_json::to_string(&wid()).unwrap();
        assert_eq!(json, r#"{"pid":42,"idx":7}"#);
        assert_eq!(serde_json::from_str::<WindowId>(&json).unwrap(), wid());
    }

    #[test]
    fn window_id_accepts_the_seq_and_debug_string_forms() {
        assert_eq!(serde_json::from_str::<WindowId>("[42, 7]").unwrap(), wid());
        assert_eq!(
            serde_json::from_str::<WindowId>(r#""WindowId { pid: 42, idx: 7 }""#).unwrap(),
            wid()
        );
        assert_eq!(WindowId::from_debug_string(&wid().to_debug_string()), Some(wid()));
        assert_eq!(WindowId::from_debug_string("Window { pid: 1, idx: 2 }"), None);
    }

    #[test]
    fn window_id_rejects_a_zero_index_and_ignores_unknown_fields() {
        assert!(serde_json::from_str::<WindowId>(r#"{"pid":1,"idx":0}"#).is_err());
        assert!(serde_json::from_str::<WindowId>("[1, 0]").is_err());
        assert_eq!(
            serde_json::from_str::<WindowId>(r#"{"pid":42,"idx":7,"extra":true}"#).unwrap(),
            wid()
        );
    }

    #[test]
    fn window_id_round_trips_through_ron() {
        let ron = ron::to_string(&wid()).unwrap();
        assert_eq!(ron::from_str::<WindowId>(&ron).unwrap(), wid());
    }

    #[test]
    fn window_id_maps_onto_the_protocol_id_and_the_server_id() {
        let protocol: rini_protocol::WindowId = wid().into();
        assert_eq!((protocol.pid, protocol.idx), (42, 7));
        assert_eq!(WindowServerId::from(wid()).as_u32(), 7);
    }

    #[test]
    fn server_id_zero_is_not_a_window() {
        assert_eq!(WindowServerId::new(0).as_nonzero(), None);
        assert_eq!(WindowServerId::new(9).as_nonzero().map(|n| n.get()), Some(9));
        assert_eq!(u32::from(WindowServerId::new(9)), 9);
    }

    fn ids() -> Vec<WindowId> {
        vec![
            WindowId::new(1, 1),
            WindowId::new(2, 1),
            WindowId::new(2, 2),
            WindowId::new(2, u32::MAX),
            WindowId::new(3, 1),
        ]
    }

    #[test]
    fn remove_all_for_pid_takes_exactly_that_pid_from_a_set() {
        let mut set: BTreeSet<WindowId> = ids().into_iter().collect();
        let removed = set.remove_all_for_pid(2);
        assert_eq!(removed.len(), 3);
        assert!(removed.iter().all(|w| w.pid == 2));
        assert_eq!(set.len(), 2);
        assert!(set.iter().all(|w| w.pid != 2));
    }

    #[test]
    fn remove_all_for_pid_takes_exactly_that_pid_from_a_map() {
        let mut map: BTreeMap<WindowId, u8> = ids().into_iter().map(|w| (w, 0)).collect();
        let removed = map.remove_all_for_pid(2);
        assert_eq!(removed.len(), 3);
        assert_eq!(map.len(), 2);
        assert!(map.remove_all_for_pid(9).is_empty());
        assert_eq!(map.len(), 2);
    }
}
