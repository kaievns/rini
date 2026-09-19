//! Transitional home of `SpaceId` until `rini-displays` exists. See `docs/architecture.md`.
use serde::{Deserialize, Serialize};
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
    #[test]
    fn space_id_serializes_as_a_bare_number() {
        let space = SpaceId::new(1234);
        assert_eq!(serde_json::to_string(&space).unwrap(), "1234");
        assert_eq!(serde_json::from_str::<SpaceId>("1234").unwrap(), space);
        assert_eq!(space.to_string(), "1234");
        assert_eq!(u64::from(space), 1234);
    }
}
