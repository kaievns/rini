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

use objc2::rc::Retained;
use objc2_app_kit::NSRunningApplication;

use crate::windows::domain::info::{WindowInfo, WindowServerInfo};

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

    /// Whether the application has quit.
    ///
    /// Asked when a request fails with "could not complete": from a live application that means busy
    /// and is ignored, and from a dead one it means the thread should stop. The two are
    /// indistinguishable from the error alone.
    fn app_has_quit(&self) -> bool;

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

    /// Everything rini records about a window, read in one go.
    ///
    /// A composition of the reads above plus a window-server lookup for the owning bundle, kept on the
    /// world so a fake answers it the same way it answers the parts.
    fn window_info(
        &self,
        elem: &Self::Element,
        hint: Option<WindowServerInfo>,
    ) -> Result<(WindowInfo, Option<WindowServerInfo>), AxError>;

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
    running_app: Retained<NSRunningApplication>,
    observer: Observer,
    enhanced_ui: EnhancedUi,
}

impl MacAx {
    pub fn new(
        app: AXUIElement,
        running_app: Retained<NSRunningApplication>,
        observer: Observer,
    ) -> Self {
        Self {
            app,
            running_app,
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

    fn app_has_quit(&self) -> bool {
        self.running_app.isTerminated()
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

    fn window_info(
        &self,
        elem: &AXUIElement,
        hint: Option<WindowServerInfo>,
    ) -> Result<(WindowInfo, Option<WindowServerInfo>), AxError> {
        WindowInfo::from_ax_element(elem, hint)
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

/// A world a test writes the answers for.
///
/// Elements are plain numbers, so a test can name one without a running application. Every answer
/// comes from a map the test filled; an element with nothing recorded answers
/// `AXError::Ax(InvalidUIElement)`, which is how Accessibility reports an element that has gone, so
/// the default for "a window the test never set up" is the same as for one that died.
///
/// Writes are recorded rather than performed. `positions` and `sizes` are what the actor asked for,
/// which is the only thing a test can check: whether a frame write happened, and with what.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct FakeAx {
    /// The application element. Windows are any other number.
    pub app: u32,
    pub windows: Vec<u32>,
    pub frames: std::collections::HashMap<u32, CGRect>,
    pub roles: std::collections::HashMap<u32, String>,
    pub subroles: std::collections::HashMap<u32, String>,
    pub titles: std::collections::HashMap<u32, String>,
    pub minimized: std::collections::HashMap<u32, bool>,
    pub server_ids: std::collections::HashMap<u32, WindowServerId>,
    pub main_window: Option<u32>,
    pub frontmost: bool,
    /// Elements whose `AXTitleUIElement` cannot be read.
    pub without_title_element: Vec<u32>,
    /// What the actor asked for, in order.
    pub positions: std::cell::RefCell<Vec<(u32, CGPoint)>>,
    pub sizes: std::cell::RefCell<Vec<(u32, CGSize)>>,
    pub raised: std::cell::RefCell<Vec<u32>>,
    pub watched: std::cell::RefCell<Vec<(u32, &'static str)>>,
    pub unwatched: std::cell::RefCell<Vec<(u32, &'static str)>>,
    pub enhanced_ui_depth: i32,
    /// The application has quit.
    pub has_quit: bool,
    /// Every read fails with "could not complete", which is an application under load.
    pub busy: bool,
}

#[cfg(test)]
impl FakeAx {
    /// A world with one application and one standard window at `frame`.
    pub fn with_one_window(window: u32, frame: CGRect) -> Self {
        let mut ax = Self {
            app: 1,
            windows: vec![window],
            frontmost: true,
            ..Self::default()
        };
        ax.describe_standard_window(window, frame);
        ax
    }

    /// Record the attributes that make `window` a standard, non-minimized window.
    pub fn describe_standard_window(&mut self, window: u32, frame: CGRect) {
        self.frames.insert(window, frame);
        self.roles.insert(window, super::element::AX_WINDOW_ROLE.to_owned());
        self.subroles
            .insert(window, super::element::AX_STANDARD_WINDOW_SUBROLE.to_owned());
        self.titles.insert(window, format!("window {window}"));
        self.minimized.insert(window, false);
        self.server_ids.insert(window, WindowServerId::new(window));
    }

    fn gone<T>() -> Result<T, AxError> {
        Err(AxError::Ax(
            objc2_application_services::AXError::InvalidUIElement,
        ))
    }

    fn busy<T>() -> Result<T, AxError> {
        Err(AxError::Ax(objc2_application_services::AXError::CannotComplete))
    }

    fn read<T: Clone>(
        &self,
        map: &std::collections::HashMap<u32, T>,
        elem: &u32,
    ) -> Result<T, AxError> {
        if self.busy {
            return Self::busy();
        }
        map.get(elem).cloned().ok_or(AxError::Ax(
            objc2_application_services::AXError::InvalidUIElement,
        ))
    }
}

#[cfg(test)]
impl AxWorld for FakeAx {
    type Element = u32;

    fn app(&self) -> u32 {
        self.app
    }

    fn windows(&self, _app: &u32) -> Result<Vec<u32>, AxError> {
        Ok(self.windows.clone())
    }

    fn main_window(&self, _app: &u32) -> Result<u32, AxError> {
        self.main_window.ok_or(AxError::NotFound)
    }

    fn frontmost(&self, _app: &u32) -> Result<bool, AxError> {
        Ok(self.frontmost)
    }

    fn app_has_quit(&self) -> bool {
        self.has_quit
    }

    fn frame(&self, elem: &u32) -> Result<CGRect, AxError> {
        self.read(&self.frames, elem)
    }

    fn set_position(&self, elem: &u32, position: CGPoint) -> Result<(), AxError> {
        if !self.frames.contains_key(elem) {
            return Self::gone();
        }
        self.positions.borrow_mut().push((*elem, position));
        Ok(())
    }

    fn set_size(&self, elem: &u32, size: CGSize) -> Result<(), AxError> {
        if !self.frames.contains_key(elem) {
            return Self::gone();
        }
        self.sizes.borrow_mut().push((*elem, size));
        Ok(())
    }

    fn role(&self, elem: &u32) -> Result<String, AxError> {
        self.read(&self.roles, elem)
    }

    fn subrole(&self, elem: &u32) -> Result<String, AxError> {
        self.read(&self.subroles, elem)
    }

    fn title(&self, elem: &u32) -> Result<String, AxError> {
        self.read(&self.titles, elem)
    }

    fn minimized(&self, elem: &u32) -> Result<bool, AxError> {
        self.read(&self.minimized, elem)
    }

    fn parent(&self, _elem: &u32) -> Result<Option<u32>, AxError> {
        Ok(Some(self.app))
    }

    fn raise(&self, elem: &u32) -> Result<(), AxError> {
        if !self.frames.contains_key(elem) {
            return Self::gone();
        }
        self.raised.borrow_mut().push(*elem);
        Ok(())
    }

    fn can_resize(&self, _elem: &u32) -> Result<bool, AxError> {
        Ok(true)
    }

    fn modal(&self, _elem: &u32) -> Result<bool, AxError> {
        Ok(false)
    }

    fn window_info(
        &self,
        elem: &u32,
        hint: Option<WindowServerInfo>,
    ) -> Result<(WindowInfo, Option<WindowServerInfo>), AxError> {
        let role = self.role(elem)?;
        let subrole = self.subrole(elem)?;
        let info = WindowInfo {
            is_standard: role == super::element::AX_WINDOW_ROLE
                && subrole == super::element::AX_STANDARD_WINDOW_SUBROLE,
            is_root: true,
            is_minimized: self.minimized(elem).unwrap_or(false),
            is_resizable: true,
            min_size: None,
            max_size: None,
            title: self.title(elem).unwrap_or_default(),
            frame: self.frame(elem)?,
            sys_id: self.window_server_id(elem),
            bundle_id: None,
            path: None,
            ax_role: Some(role),
            ax_subrole: Some(subrole),
            is_modal: false,
        };
        Ok((info, hint))
    }

    fn window_server_id(&self, elem: &u32) -> Option<WindowServerId> {
        self.server_ids.get(elem).copied()
    }

    fn watch(&self, elem: &u32, notification: &'static str, _data: usize) -> Result<(), AxError> {
        if !self.frames.contains_key(elem) && *elem != self.app {
            return Self::gone();
        }
        self.watched.borrow_mut().push((*elem, notification));
        Ok(())
    }

    fn unwatch(&self, elem: &u32, notification: &'static str) {
        self.unwatched.borrow_mut().push((*elem, notification));
    }

    fn read_attribute(&self, elem: &u32, name: &'static str) -> Result<(), AxError> {
        if name == "AXTitleUIElement" && self.without_title_element.contains(elem) {
            return Self::gone();
        }
        Ok(())
    }

    fn suppress_enhanced_ui(&mut self) {
        self.enhanced_ui_depth += 1;
    }

    fn restore_enhanced_ui(&mut self) {
        self.enhanced_ui_depth -= 1;
    }

    fn restore_enhanced_ui_if_needed(&mut self) {
        self.enhanced_ui_depth = 0;
    }
}
