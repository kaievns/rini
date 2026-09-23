//! Everything the per-app thread asks of Accessibility, behind one trait.
//!
//! `app_actor.rs` is the largest file in the tree with no tests of its own, and the reason is that it
//! cannot take a step without a live `AXUIElement`: it stores them, keys a map by them, and reads and
//! writes windows through them. No amount of rearranging makes that testable — the dependency has to
//! become a parameter.
//!
//! So `State` is generic over an `AxWorld`. Production installs [`MacAx`], which is the calls the
//! actor used to make inline. A test installs a fake whose elements are plain numbers and whose
//! answers it wrote itself, and the actor's own logic runs unchanged.
//!
//! Generics rather than `Box<dyn AxWorld>` on purpose: every frame write and every notification goes
//! through here, so an allocation and a vtable hop per call is not free, and nothing needs two worlds
//! in one process.

use std::fmt::Debug;
use std::hash::Hash;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use rini_core::ids::WindowServerId;

use super::element::{AXUIElement, Error as AxError};
use super::enhanced_ui::EnhancedUi;
use super::observer::Observer;

/// One window or application, as the world in use names it.
pub trait AxElement: Clone + Eq + Hash + Debug {}
impl<T: Clone + Eq + Hash + Debug> AxElement for T {}

/// The Accessibility surface the per-app thread uses.
///
/// Every method that can fail returns [`AxError`], which the actor classifies through
/// `windows::domain::ax_events`. The trait deliberately answers in rini's types rather than handing
/// back Accessibility values to be unwrapped at the call site.
pub trait AxWorld {
    /// How this world names a window or an application.
    type Element: AxElement;

    /// The application element this world is for.
    fn app(&self) -> Self::Element;

    /// The application's windows, in the order Accessibility lists them.
    ///
    /// Space-filtered by macOS: a window on another space is absent, which is NOT the same as gone.
    fn windows(&self, app: &Self::Element) -> Result<Vec<Self::Element>, AxError>;

    fn main_window(&self, app: &Self::Element) -> Result<Self::Element, AxError>;
    fn frontmost(&self, app: &Self::Element) -> Result<bool, AxError>;

    fn frame(&self, elem: &Self::Element) -> Result<CGRect, AxError>;
    fn set_position(&self, elem: &Self::Element, position: CGPoint) -> Result<(), AxError>;
    fn set_size(&self, elem: &Self::Element, size: CGSize) -> Result<(), AxError>;

    fn role(&self, elem: &Self::Element) -> Result<String, AxError>;
    fn subrole(&self, elem: &Self::Element) -> Result<String, AxError>;
    fn title(&self, elem: &Self::Element) -> Result<String, AxError>;
    fn minimized(&self, elem: &Self::Element) -> Result<bool, AxError>;
    /// The element's parent, or `None` when Accessibility reports it has none.
    fn parent(&self, elem: &Self::Element) -> Result<Option<Self::Element>, AxError>;
    fn raise(&self, elem: &Self::Element) -> Result<(), AxError>;
    fn can_resize(&self, elem: &Self::Element) -> Result<bool, AxError>;
    fn modal(&self, elem: &Self::Element) -> Result<bool, AxError>;

    /// The window server's own id for this element, if it has one.
    ///
    /// Absent for a window Accessibility knows about before the window server does, which is the
    /// ordinary case for a window that has just been created.
    fn window_server_id(&self, elem: &Self::Element) -> Option<WindowServerId>;

    /// Start delivering `notification` for `elem`, carrying `data` back to the callback.
    fn watch(
        &self,
        elem: &Self::Element,
        notification: &'static str,
        data: usize,
    ) -> Result<(), AxError>;

    /// Stop delivering `notification` for `elem`.
    fn unwatch(&self, elem: &Self::Element, notification: &'static str);

    /// Whether `name` can be READ from this element. The value is not wanted.
    ///
    /// `Ok` means the read worked, whether or not there was a value; `Err` means it failed. That
    /// distinction is the whole content at the one call site: a window with no readable
    /// `AXTitleUIElement` is not a standard window for the applications
    /// `admissible::needs_title_element_to_be_standard` names.
    fn read_attribute(&self, elem: &Self::Element, name: &'static str) -> Result<(), AxError>;

    /// Suppress the application's Enhanced User Interface for the duration of a batch of writes.
    ///
    /// Refcounted: nested acquisitions are one suppression. AppKit re-lays-out a window when this is
    /// on, which fights a frame write, so it is held across a whole burst rather than per write.
    fn suppress_enhanced_ui(&mut self);

    /// Release one suppression acquired by [`AxWorld::suppress_enhanced_ui`].
    fn restore_enhanced_ui(&mut self);

    /// Put Enhanced User Interface back if this world still has it suppressed. Called on teardown.
    fn restore_enhanced_ui_if_needed(&mut self);
}

/// The real thing: Accessibility on this machine.
///
/// Owns the application element and the observer, because the two are inseparable in practice — a
/// notification is registered against an element and delivered through an observer bound to the same
/// process.
pub struct MacAx {
    app: AXUIElement,
    observer: Observer,
    enhanced_ui: EnhancedUi,
}

impl MacAx {
    pub fn new(app: AXUIElement, observer: Observer) -> Self {
        Self {
            app,
            observer,
            enhanced_ui: EnhancedUi::default(),
        }
    }

    pub fn observer(&self) -> &Observer {
        &self.observer
    }
}

impl AxWorld for MacAx {
    type Element = AXUIElement;

    fn app(&self) -> AXUIElement {
        self.app.clone()
    }

    fn windows(&self, app: &AXUIElement) -> Result<Vec<AXUIElement>, AxError> {
        app.windows()
    }

    fn main_window(&self, app: &AXUIElement) -> Result<AXUIElement, AxError> {
        app.main_window()
    }

    fn frontmost(&self, app: &AXUIElement) -> Result<bool, AxError> {
        app.frontmost()
    }

    fn frame(&self, elem: &AXUIElement) -> Result<CGRect, AxError> {
        elem.frame()
    }

    fn set_position(&self, elem: &AXUIElement, position: CGPoint) -> Result<(), AxError> {
        elem.set_position(position)
    }

    fn set_size(&self, elem: &AXUIElement, size: CGSize) -> Result<(), AxError> {
        elem.set_size(size)
    }

    fn role(&self, elem: &AXUIElement) -> Result<String, AxError> {
        elem.role()
    }

    fn subrole(&self, elem: &AXUIElement) -> Result<String, AxError> {
        elem.subrole()
    }

    fn title(&self, elem: &AXUIElement) -> Result<String, AxError> {
        elem.title()
    }

    fn minimized(&self, elem: &AXUIElement) -> Result<bool, AxError> {
        elem.minimized()
    }

    fn parent(&self, elem: &AXUIElement) -> Result<Option<AXUIElement>, AxError> {
        elem.parent()
    }

    fn raise(&self, elem: &AXUIElement) -> Result<(), AxError> {
        elem.raise()
    }

    fn can_resize(&self, elem: &AXUIElement) -> Result<bool, AxError> {
        elem.can_resize()
    }

    fn modal(&self, elem: &AXUIElement) -> Result<bool, AxError> {
        elem.modal()
    }

    fn window_server_id(&self, elem: &AXUIElement) -> Option<WindowServerId> {
        WindowServerId::try_from(elem).ok()
    }

    fn watch(
        &self,
        elem: &AXUIElement,
        notification: &'static str,
        data: usize,
    ) -> Result<(), AxError> {
        self.observer.add_notification_with_data(elem, notification, data)
    }

    fn unwatch(&self, elem: &AXUIElement, notification: &'static str) {
        let _ = self.observer.remove_notification(elem, notification);
    }

    fn read_attribute(&self, elem: &AXUIElement, name: &'static str) -> Result<(), AxError> {
        elem.attribute(name).map(|_| ())
    }

    fn suppress_enhanced_ui(&mut self) {
        let app = self.app.clone();
        self.enhanced_ui.acquire(&app);
    }

    fn restore_enhanced_ui(&mut self) {
        let app = self.app.clone();
        self.enhanced_ui.release(&app);
    }

    fn restore_enhanced_ui_if_needed(&mut self) {
        let app = self.app.clone();
        self.enhanced_ui.restore_if_needed(&app);
    }
}
