//! Which way a picture was taken: what decides whether it carries its window's blur, whether its
//! corners hold what was behind the window, and what the journal records of every picture a tile
//! wears.

use serde::Serialize;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// Captured fresh from the framebuffer through SkyLight, the window on its own or a composite of
    /// windows listed together. Only valid for a fully visible window.
    #[serde(rename = "skylight")]
    SkyLight,
    /// SkyLight's baked call: the window composited over what was below it and the desktop, so a
    /// material carries its blur and the corners hold the wallpaper.
    Baked,
    /// Captured from the window's own surface. Valid at any visibility, but expensive.
    #[serde(rename = "screencapturekit")]
    ScreenCaptureKit,
    /// Not a capture: the dark stand-in a window with no picture flies with, so its slot is never a
    /// hole. Never cached.
    StandIn,
}

impl Route {
    /// Whether the overlay clips a picture taken this way to its tile's corner: the stand-in has no
    /// corners of its own, and a baked picture has the wallpaper in its corners.
    pub fn clips(self) -> bool {
        matches!(self, Self::StandIn | Self::Baked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_stand_in_and_a_baked_picture_are_clipped() {
        assert!(Route::StandIn.clips());
        assert!(Route::Baked.clips());
        assert!(!Route::SkyLight.clips());
        assert!(!Route::ScreenCaptureKit.clips());
    }
}
