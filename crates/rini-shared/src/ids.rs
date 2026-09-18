//! Identities shared by every crate. See `docs/architecture.md`.

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

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct SpaceId(u64);

impl SpaceId {
    pub fn new(id: u64) -> SpaceId {
        SpaceId(id)
    }

    pub fn get(&self) -> u64 {
        self.0
    }
}

impl From<SpaceId> for u64 {
    fn from(id: SpaceId) -> u64 {
        id.get()
    }
}

impl std::fmt::Display for SpaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
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

    #[test]
    fn space_id_serializes_as_a_bare_number() {
        let space = SpaceId::new(1234);
        assert_eq!(serde_json::to_string(&space).unwrap(), "1234");
        assert_eq!(serde_json::from_str::<SpaceId>("1234").unwrap(), space);
        assert_eq!(space.to_string(), "1234");
        assert_eq!(u64::from(space), 1234);
    }
}
