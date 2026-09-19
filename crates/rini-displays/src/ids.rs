//! Display and space identity. `SpaceId` is the window server's own id, declared with the API
//! that mints it; `ScreenId` is the CGDirectDisplayID.
use serde::{Deserialize, Serialize};

pub use rini_skylight_sys::SpaceId;

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScreenId(u32);
impl ScreenId {
    pub fn new(id: u32) -> Self {
        ScreenId(id)
    }

    pub fn as_u32(&self) -> u32 {
        self.0
    }
}
