//! The thread that pictures macOS's menu extras for the bar: every extra in one capture a tick, cut
//! apart, compared with what was last sent, and sent on only when something changed. See
//! `src/bar/docs/menu-extras.md`.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

use objc2_app_kit::NSStatusWindowLevel;
use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGContext, CGDisplayBounds, CGError,
    CGImage, CGMainDisplayID, CGWindowListOption, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer,
    kCGWindowName, kCGWindowNumber, kCGWindowOwnerName,
};
use rini_skylight_sys::{G_CONNECTION, SLSCaptureWindowsContentsToRectWithOptions};
use tracing::debug;

use crate::animation::platform::edge_dressing::rgba_bitmap_context;
use crate::bar::domain::extras::{self, Command, Composite, Kind, Schedule, Stamp, StatusWindow};
use crate::windows::platform::window_server::{
    bounds_from_dict, get_num, get_string, get_windows_raw,
};

/// What sketchybar passes. `1 << 11` would crop each picture to its opaque bounds and lose where the
/// icon sits.
const CAPTURE_OPTIONS: u32 = 1 << 8;

/// One extra's picture.
pub struct Extra {
    /// The status window's id, stable for as long as the extra is.
    pub window: u32,
    /// `Kind::Vital` or `Kind::Tray`, never `Kind::Skip`.
    pub kind: Kind,
    /// At `scale` pixels per point, with a transparent ground.
    pub image: CFRetained<CGImage>,
    pub scale: f64,
    /// Where its ink starts and ends, in points from the picture's left edge.
    pub ink: (f64, f64),
}

/// Every extra drawn, in menu-bar order, left to right.
#[derive(Default)]
pub struct Extras {
    pub items: Vec<Extra>,
}

/// The running thread. It ends when this is dropped.
pub struct Watcher {
    commands: mpsc::Sender<Command>,
}

impl Watcher {
    /// Starts the thread. `send` is called from it with the extras whenever a picture changed.
    pub fn spawn(send: Box<dyn Fn(Extras) + Send>) -> Watcher {
        let (commands, inbox) = mpsc::channel();
        thread::Builder::new()
            .name("bar-extras".to_string())
            .spawn(move || run(&inbox, &*send))
            .expect("failed to spawn bar-extras thread");
        Watcher { commands }
    }

    /// No captures while paused: during a flight, and while no bar is up.
    pub fn set_paused(&self, paused: bool) {
        let _ = self.commands.send(Command::Pause(paused));
    }

    /// Picture now rather than at the next tick.
    pub fn refresh(&self) {
        let _ = self.commands.send(Command::Refresh);
    }
}

fn run(inbox: &Receiver<Command>, send: &dyn Fn(Extras)) {
    let mut schedule = Schedule::new(Instant::now());
    let mut last: Option<Vec<Stamp>> = None;
    loop {
        let command = match schedule.wait(Instant::now()) {
            Some(timeout) => inbox.recv_timeout(timeout),
            None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match command {
            Ok(command) => schedule.on(command, Instant::now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        for command in inbox.try_iter() {
            schedule.on(command, Instant::now());
        }
        if !schedule.take(Instant::now()) {
            continue;
        }
        let Some((extras, stamps)) = picture() else {
            continue;
        };
        if extras::changed(last.as_deref(), &stamps) {
            last = Some(stamps);
            send(extras);
        }
    }
}

/// Where a status window is, beside what `select` reads of it.
#[derive(Clone, Copy)]
struct Placed {
    window: u32,
    bounds: CGRect,
}

/// Every extra drawn and what it is compared by, or `None` when the capture failed.
fn picture() -> Option<(Extras, Vec<Stamp>)> {
    let (windows, placed) = status_windows();
    let chosen: Vec<(Placed, Kind)> =
        extras::select(&windows).into_iter().map(|(index, kind)| (placed[index], kind)).collect();
    if chosen.is_empty() {
        return Some((Extras::default(), Vec::new()));
    }
    let ids: Vec<u32> = chosen.iter().map(|(placed, _)| placed.window).collect();
    let bounds: Vec<CGRect> = chosen.iter().map(|(placed, _)| placed.bounds).collect();
    let image = capture(&ids)?;
    let composite = Composite::new(&bounds, (CGImage::width(Some(&image)), CGImage::height(Some(&image))))?;
    cut(&image, composite, &chosen)
}

/// The status-level windows on the main display's menu bar, and where each is.
fn status_windows() -> (Vec<StatusWindow>, Vec<Placed>) {
    let display = CGDisplayBounds(CGMainDisplayID());
    get_windows_raw::<CFDictionary<CFString, CFType>>(CGWindowListOption::OptionAll, kCGNullWindowID)
        .iter()
        .filter(|window| get_num(window, unsafe { kCGWindowLayer }) == Some(NSStatusWindowLevel as i64))
        .filter_map(|window| {
            let id = get_num(&window, unsafe { kCGWindowNumber })? as u32;
            let bounds = window
                .get(unsafe { kCGWindowBounds })
                .and_then(|bounds| bounds.downcast::<CFDictionary>().ok())
                .and_then(bounds_from_dict)?;
            let status = StatusWindow {
                owner: get_string(&window, unsafe { kCGWindowOwnerName }).unwrap_or_default(),
                name: get_string(&window, unsafe { kCGWindowName }).unwrap_or_default(),
                x: bounds.origin.x,
                width: bounds.size.width,
            };
            Some((status, Placed { window: id, bounds }))
        })
        .filter(|(status, _)| extras::on_display(status, display))
        .unzip()
}

/// One picture of every window in `ids`, over the union of their bounds.
fn capture(ids: &[u32]) -> Option<CFRetained<CGImage>> {
    // CGRectNull, which the bindings leave out.
    let null = CGRect::new(CGPoint::new(f64::INFINITY, f64::INFINITY), CGSize::ZERO);
    let mut image: *mut CGImage = std::ptr::null_mut();
    // SAFETY: `ids` outlives the call and holds `ids.len()` window ids; the image comes back retained.
    let err = unsafe {
        SLSCaptureWindowsContentsToRectWithOptions(
            *G_CONNECTION,
            ids.as_ptr(),
            ids.len() as i32,
            null,
            CAPTURE_OPTIONS,
            &mut image,
        )
    };
    // SAFETY: a non-null image is ours to release.
    let image = std::ptr::NonNull::new(image).map(|image| unsafe { CFRetained::from_raw(image) });
    if err != CGError::Success {
        debug!("bar-extras: capture of {} windows failed: {err:?}", ids.len());
        return None;
    }
    image
}

/// Each chosen window's picture cut out of the capture, measured and fingerprinted. An extra with no
/// ink is left out. `None` when the capture's pixels cannot be read.
fn cut(image: &CGImage, composite: Composite, chosen: &[(Placed, Kind)]) -> Option<(Extras, Vec<Stamp>)> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let ctx = rgba_bitmap_context(width, height)?;
    let whole = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64));
    CGContext::draw_image(Some(&ctx), whole, Some(image));
    let data = CGBitmapContextGetData(Some(&ctx)) as *const u8;
    if data.is_null() {
        return None;
    }
    let stride = CGBitmapContextGetBytesPerRow(Some(&ctx));
    // SAFETY: the context owns `stride * height` bytes and outlives this slice.
    let pixels = unsafe { std::slice::from_raw_parts(data, stride * height) };
    let mut items = Vec::with_capacity(chosen.len());
    let mut stamps = Vec::with_capacity(chosen.len());
    for &(Placed { window, bounds }, kind) in chosen {
        let Some(rect) = composite.crop(bounds) else {
            continue;
        };
        let Some(ink) = extras::ink_span(&extras::column_alpha(pixels, stride, rect), composite.scale)
        else {
            continue;
        };
        let Some(picture) = CGImage::with_image_in_rect(Some(image), rect.rect()) else {
            continue;
        };
        stamps.push(Stamp { window, kind, hash: extras::fingerprint(pixels, stride, rect) });
        items.push(Extra { window, kind, image: picture, scale: composite.scale, ink });
    }
    Some((Extras { items }, stamps))
}
