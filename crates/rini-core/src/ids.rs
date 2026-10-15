//! Every identity rini speaks.
//!
//! `WindowId` is rini's own: a pid plus a per-process index, valid for the lifetime of the owning
//! process. `ScreenId` is the CGDirectDisplayID, `DisplayUuid` the display's identity across
//! reconnects and restarts. `WindowServerId` (CGWindowID) and `SpaceId`
//! (CGSSpaceID) are the window server's, declared with the private API that mints them and
//! re-exported here so a feature does not reach into `rini-skylight-sys` for a number.

use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::num::NonZeroU32;

pub use libc::pid_t;
pub use rini_skylight_sys::{SpaceId, WindowServerId};

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
    where S: serde::ser::Serializer {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("WindowId", 2)?;
        s.serialize_field("pid", &self.pid)?;
        s.serialize_field("idx", &self.idx.get())?;
        s.end()
    }
}

impl<'de> serde::de::Deserialize<'de> for WindowId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: serde::de::Deserializer<'de> {
        struct WindowIdVisitor;
        impl<'de> serde::de::Visitor<'de> for WindowIdVisitor {
            type Value = WindowId;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a WindowId struct (with fields `pid` and `idx`), a tuple/seq (pid, idx), or a debug string like `WindowId { pid: 123, idx: 456 }`",
                )
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where E: serde::de::Error {
                WindowId::from_debug_string(v)
                    .ok_or_else(|| E::custom("invalid WindowId debug string"))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<WindowId, A::Error>
            where A: serde::de::SeqAccess<'de> {
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
            where M: serde::de::MapAccess<'de> {
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
}

impl From<WindowId> for WindowServerId {
    fn from(id: WindowId) -> Self { Self(id.idx.into()) }
}

/// A display, keyed by its CGDirectDisplayID.
#[derive(
    PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone, Copy, serde::Serialize, serde::Deserialize,
)]
pub struct ScreenId(u32);

impl ScreenId {
    pub fn new(id: u32) -> Self { ScreenId(id) }

    pub fn as_u32(&self) -> u32 { self.0 }
}

/// A display's stable identity: the UUID CoreGraphics reports for it, which survives a reconnect and
/// a restart where the `ScreenId` and the display's native spaces do not.
///
/// Written to `layout.ron` and printed over IPC as the bare string. Empty means not reported yet.
#[derive(
    Debug,
    Default,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct DisplayUuid(String);

impl DisplayUuid {
    pub fn new(uuid: impl Into<String>) -> Self { DisplayUuid(uuid.into()) }

    /// The name a display goes by when CoreGraphics reports no UUID for it, made from its
    /// `ScreenId`. Stable only while that id is, so not across a reconnect.
    pub fn fallback(screen: ScreenId) -> Self {
        DisplayUuid(format!("cgdisplay-{}", screen.as_u32()))
    }

    /// Whether this is a [`DisplayUuid::fallback`] rather than a UUID CoreGraphics reported.
    pub fn is_fallback(&self) -> bool { self.0.starts_with("cgdisplay-") }

    pub fn as_str(&self) -> &str { &self.0 }

    pub fn is_empty(&self) -> bool { self.0.is_empty() }
}

impl std::fmt::Display for DisplayUuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

impl PartialEq<str> for DisplayUuid {
    fn eq(&self, other: &str) -> bool { self.0 == other }
}

impl PartialEq<&str> for DisplayUuid {
    fn eq(&self, other: &&str) -> bool { self.0 == *other }
}

impl From<&str> for DisplayUuid {
    fn from(uuid: &str) -> Self { DisplayUuid::new(uuid) }
}

impl From<String> for DisplayUuid {
    fn from(uuid: String) -> Self { DisplayUuid(uuid) }
}

impl From<DisplayUuid> for String {
    fn from(uuid: DisplayUuid) -> Self { uuid.0 }
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

    fn wid() -> WindowId { WindowId::new(42, 7) }

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
        assert_eq!(WindowId::from_debug_string(&format!("{:?}", wid())), Some(wid()));
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
    fn window_id_maps_onto_the_server_id() {
        assert_eq!(WindowServerId::from(wid()).as_u32(), 7);
    }

    #[test]
    fn a_screen_id_round_trips_through_its_raw_display_id() {
        assert_eq!(ScreenId::new(69733382).as_u32(), 69733382);
    }

    #[test]
    fn server_id_zero_is_not_a_window() {
        assert_eq!(WindowServerId::new(0).as_nonzero(), None);
        assert_eq!(WindowServerId::new(9).as_nonzero().map(|n| n.get()), Some(9));
        assert_eq!(u32::from(WindowServerId::new(9)), 9);
    }

    const UUID: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";

    #[test]
    fn a_display_uuid_is_written_as_the_bare_string() {
        let uuid = DisplayUuid::new(UUID);
        assert_eq!(ron::to_string(&uuid).unwrap(), ron::to_string(UUID).unwrap());
        assert_eq!(serde_json::to_string(&uuid).unwrap(), format!("\"{UUID}\""));
        assert_eq!(
            ron::from_str::<DisplayUuid>(&format!("\"{UUID}\"")).unwrap(),
            uuid
        );
        assert_eq!(uuid.to_string(), UUID);
    }

    #[test]
    fn a_display_uuid_hashes_and_orders_as_its_string() {
        use std::hash::BuildHasher;
        let hasher = std::hash::RandomState::new();
        assert_eq!(
            hasher.hash_one(DisplayUuid::new(UUID)),
            hasher.hash_one(UUID.to_owned())
        );
        assert!(DisplayUuid::new("A") < DisplayUuid::new("B"));
    }

    #[test]
    fn a_fallback_uuid_names_the_screen_and_says_it_is_one() {
        let fallback = DisplayUuid::fallback(ScreenId::new(2));
        assert_eq!(fallback.as_str(), "cgdisplay-2");
        assert!(fallback.is_fallback());
        assert!(!DisplayUuid::new(UUID).is_fallback());
        assert!(DisplayUuid::default().is_empty());
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
}
