//! What the motion tests stage with: a window on a surface.

use objc2_core_foundation::CGRect;
use rini_core::ids::WindowId;

use super::surface::SurfaceGeometry;

/// A window on a surface, as a test stages it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OnSurface {
    pub window: WindowId,
    pub frame: CGRect,
    pub pinned: bool,
    pub floating: bool,
    pub from: Option<CGRect>,
}

impl SurfaceGeometry for OnSurface {
    fn window(&self) -> WindowId { self.window }

    fn frame(&self) -> CGRect { self.frame }

    fn pinned(&self) -> bool { self.pinned }

    fn floating(&self) -> bool { self.floating }

    fn from(&self) -> Option<CGRect> { self.from }
}
