//! `Capturer`, every read of the window server and every capture the pictures service's thread
//! makes: `MacCapturer` makes them, and under test `FakeCapturer` answers them from what a test gives
//! it and records each one asked. The service is generic over it, so its rounds run in a test with
//! no window server behind them.

use objc2_core_foundation::{CFRetained, CGRect};
use objc2_core_graphics::CGImage;
use rini_core::ids::{WindowServerId, pid_t};
use rini_geometry::space::ScreenRect;

use crate::pictures::domain::knowledge::Learned;
use crate::pictures::domain::schedule::Display;
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::platform::desktop::{
    BarStrip, DesktopBackdrop, bar_strip, desktop_backdrop_windows,
};
use crate::pictures::platform::picture::Picture;
use crate::pictures::platform::screencapturekit::{self, Prepared};
use crate::pictures::platform::skylight::{self, Batch, BatchCall, capture_list_via_skylight};
use crate::windows::platform::window_server::{key_focus_pid, server_frame};

/// What the service's thread reads and captures. The ScreenCaptureKit calls complete on their own
/// queues, through `done`.
pub trait Capturer {
    /// The frame the window server has for `id` now.
    fn server_frame(&self, id: WindowServerId) -> Option<CGRect>;
    /// The process the window server has holding the keys.
    fn key_focus_pid(&self) -> Option<pid_t>;
    /// The windows the desktop of `display` is composited from.
    fn desktop_windows(&self, display: ScreenRect) -> DesktopBackdrop;
    /// One batched SkyLight capture of windows on screen (`skylight::capture_on_screen`).
    fn capture_on_screen(
        &self,
        call: BatchCall<'_>,
        learned: &mut Learned,
        halt: &dyn Fn() -> bool,
    ) -> Batch;
    fn list(
        &self,
        targets: Vec<PictureTarget>,
        done: impl Fn(PictureTarget, Option<Prepared>) + Send + Sync + 'static,
    );
    fn capture(
        &self,
        target: PictureTarget,
        prepared: Prepared,
        done: impl Fn(Option<Picture>) + Send + Sync + 'static,
    );
    fn render_desktop(
        &self,
        display: Display,
        done: impl Fn(Option<Picture>) + Send + Sync + 'static,
    );
    /// A handover check's overlay, alone.
    fn capture_alone(&self, window: WindowServerId) -> Option<CFRetained<CGImage>>;
    /// A handover check's screen.
    fn capture_screen(&self, display: ScreenRect) -> Option<CFRetained<CGImage>>;
    /// The bar on `display` and the strip it covers (`bar_of`).
    fn bar(&self, display: Display) -> (Option<Picture>, Option<ScreenRect>);
}

/// The calls themselves.
pub struct MacCapturer;

impl Capturer for MacCapturer {
    fn server_frame(&self, id: WindowServerId) -> Option<CGRect> { server_frame(id) }

    fn key_focus_pid(&self) -> Option<pid_t> { key_focus_pid() }

    fn desktop_windows(&self, display: ScreenRect) -> DesktopBackdrop {
        desktop_backdrop_windows(display)
    }

    fn capture_on_screen(
        &self,
        call: BatchCall<'_>,
        learned: &mut Learned,
        halt: &dyn Fn() -> bool,
    ) -> Batch {
        skylight::capture_on_screen(call, learned, halt)
    }

    fn list(
        &self,
        targets: Vec<PictureTarget>,
        done: impl Fn(PictureTarget, Option<Prepared>) + Send + Sync + 'static,
    ) {
        screencapturekit::list(targets, done)
    }

    fn capture(
        &self,
        target: PictureTarget,
        prepared: Prepared,
        done: impl Fn(Option<Picture>) + Send + Sync + 'static,
    ) {
        screencapturekit::capture(target, prepared, done)
    }

    fn render_desktop(
        &self,
        display: Display,
        done: impl Fn(Option<Picture>) + Send + Sync + 'static,
    ) {
        screencapturekit::render_desktop(display, done)
    }

    fn capture_alone(&self, window: WindowServerId) -> Option<CFRetained<CGImage>> {
        skylight::capture_alone(window)
    }

    fn capture_screen(&self, display: ScreenRect) -> Option<CFRetained<CGImage>> {
        skylight::capture_screen(display)
    }

    fn bar(&self, display: Display) -> (Option<Picture>, Option<ScreenRect>) {
        bar_of(&bar_strip(display.frame), display.scale)
    }
}

/// The bar listed in `strip`, captured on its own to keep its alpha, and the strip it covers. See "The
/// bar has to be captured on its own" in `src/pictures/docs/capture-research.md`.
pub fn bar_of(strip: &BarStrip, scale: f64) -> (Option<Picture>, Option<ScreenRect>) {
    let Some(bounds) = strip.bounds else {
        return (None, None);
    };
    let picture = capture_list_via_skylight(&strip.windows, bounds.size, scale)
        .filter(|picture| picture.fits(bounds.size));
    (picture, Some(bounds))
}

#[cfg(test)]
pub use fake::FakeCapturer;

#[cfg(test)]
mod fake {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use objc2_core_foundation::{CFRetained, CGRect};
    use objc2_core_graphics::CGImage;
    use rini_core::ids::{WindowId, WindowServerId, pid_t};
    use rini_geometry::space::{FromCg, ScreenRect, ScreenSize};

    use super::Capturer;
    use crate::pictures::domain::knowledge::Learned;
    use crate::pictures::domain::schedule::Display;
    use crate::pictures::domain::target::PictureTarget;
    use crate::pictures::platform::desktop::DesktopBackdrop;
    use crate::pictures::platform::picture::{Picture, test_picture};
    use crate::pictures::platform::screencapturekit::Prepared;
    use crate::pictures::platform::skylight::{Batch, BatchCall};

    /// A window server with only the frames given, and captures that always come back, each window
    /// pictured at its frame's size and seen for the first time as translucent. Like the real one, it
    /// asks `halt` before each call and makes none once it says stop. A listing finds nothing, and no
    /// render or picture of the bar comes back.
    #[derive(Default)]
    pub struct FakeCapturer {
        pub frames: HashMap<WindowServerId, CGRect>,
        pub key: Option<pid_t>,
        pub desktop: DesktopBackdrop,
        /// Every batched call comes back another shape than its windows.
        pub misshapen: bool,
        /// Run after each batched call is made: a press asked while a round is under way.
        pub after_call: Option<Box<dyn Fn() + Send>>,
        /// Every batched call made, by the windows in it.
        pub calls: RefCell<Vec<Vec<WindowId>>>,
        pub key_reads: Cell<usize>,
        pub desktop_reads: Cell<usize>,
        pub listed: RefCell<Vec<PictureTarget>>,
        pub rendered: RefCell<Vec<Display>>,
        pub bar_reads: Cell<usize>,
    }

    impl Capturer for FakeCapturer {
        fn server_frame(&self, id: WindowServerId) -> Option<CGRect> {
            self.frames.get(&id).copied()
        }

        fn key_focus_pid(&self) -> Option<pid_t> {
            self.key_reads.set(self.key_reads.get() + 1);
            self.key
        }

        fn desktop_windows(&self, _display: ScreenRect) -> DesktopBackdrop {
            self.desktop_reads.set(self.desktop_reads.get() + 1);
            self.desktop.clone()
        }

        fn capture_on_screen(
            &self,
            call: BatchCall<'_>,
            learned: &mut Learned,
            halt: &dyn Fn() -> bool,
        ) -> Batch {
            let windows: Vec<WindowId> = call.windows.iter().map(|window| window.window).collect();
            if halt() {
                return Batch {
                    pictures: Vec::new(),
                    desktop_stale: false,
                    halted: windows,
                };
            }
            self.calls.borrow_mut().push(windows);
            let pictures = call
                .windows
                .iter()
                .map(|window| {
                    if learned.get(window.server_id).translucent.is_none() {
                        learned.saw_alpha(window.server_id, true);
                    }
                    (
                        window.window,
                        test_picture(ScreenSize::from_cg(window.frame.size)),
                    )
                })
                .collect();
            if let Some(after_call) = &self.after_call {
                after_call();
            }
            Batch {
                pictures,
                desktop_stale: self.misshapen,
                halted: Vec::new(),
            }
        }

        fn list(
            &self,
            targets: Vec<PictureTarget>,
            done: impl Fn(PictureTarget, Option<Prepared>) + Send + Sync + 'static,
        ) {
            for target in &targets {
                done(*target, None);
            }
            self.listed.borrow_mut().extend(targets);
        }

        fn capture(
            &self,
            _target: PictureTarget,
            _prepared: Prepared,
            _done: impl Fn(Option<Picture>) + Send + Sync + 'static,
        ) {
        }

        fn render_desktop(
            &self,
            display: Display,
            _done: impl Fn(Option<Picture>) + Send + Sync + 'static,
        ) {
            self.rendered.borrow_mut().push(display);
        }

        fn capture_alone(&self, _window: WindowServerId) -> Option<CFRetained<CGImage>> { None }

        fn capture_screen(&self, _display: ScreenRect) -> Option<CFRetained<CGImage>> { None }

        fn bar(&self, _display: Display) -> (Option<Picture>, Option<ScreenRect>) {
            self.bar_reads.set(self.bar_reads.get() + 1);
            (None, None)
        }
    }
}
