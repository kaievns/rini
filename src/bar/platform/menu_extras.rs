//! The thread that pictures what the bar shows of macOS: every menu extra in one capture a tick, cut
//! apart, compared with what was last sent, and sent on only when something changed; and the desktop
//! behind each bar, for its ground, once a minute. See `src/bar/docs/menu-extras.md`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::collections::HashMap;
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
use crate::bar::domain::ground::Timer;
use crate::bar::platform::ground::{Ground, picture_ground};
use crate::displays::screen::active_menu_bar_display_id;
use crate::windows::platform::window_server::{
    bounds_from_dict, get_num, get_string, get_windows_raw,
};

/// What sketchybar passes. `1 << 11` would crop each picture to its opaque bounds and lose where the
/// icon sits.
const CAPTURE_OPTIONS: u32 = 1 << 8;

/// One extra's picture.
#[derive(Clone)]
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

/// What the thread is told.
enum Message {
    Extras(Command),
    /// The displays to picture the ground behind, by uuid, with their bounds.
    Grounds(Vec<(String, CGRect)>),
}

/// Where the thread's pictures go.
pub struct Sinks {
    pub extras: Box<dyn Fn(Extras) + Send>,
    pub ground: Box<dyn Fn(Ground) + Send>,
}

/// The running thread. It ends when this is dropped.
pub struct Watcher {
    commands: mpsc::Sender<Message>,
    /// Read by the thread just before it captures, so a pause stops a pass already under way.
    paused: Arc<AtomicBool>,
}

impl Watcher {
    /// Starts the thread. The sinks are called from it whenever a picture changed.
    pub fn spawn(sinks: Sinks) -> Watcher {
        let (commands, inbox) = mpsc::channel();
        let paused = Arc::new(AtomicBool::new(false));
        let read = Arc::clone(&paused);
        thread::Builder::new()
            .name("bar-extras".to_string())
            .spawn(move || run(&inbox, &read, &sinks))
            .expect("failed to spawn bar-extras thread");
        Watcher { commands, paused }
    }

    /// No captures while paused: during a flight, and while no bar is up.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        let _ = self.commands.send(Message::Extras(Command::Pause(paused)));
    }

    /// Picture now rather than at the next tick, the grounds too.
    pub fn refresh(&self) {
        let _ = self.commands.send(Message::Extras(Command::Refresh));
    }

    /// The displays whose grounds to picture, pictured at the next chance.
    pub fn set_grounds(&self, displays: Vec<(String, CGRect)>) {
        let _ = self.commands.send(Message::Grounds(displays));
    }
}

/// What was last sent, kept so an extra that did not change is sent with the same picture.
struct Sent {
    stamps: Vec<Stamp>,
    items: Vec<Extra>,
}

fn run(inbox: &Receiver<Message>, paused: &AtomicBool, sinks: &Sinks) {
    let mut schedule = Schedule::new(Instant::now());
    let mut last: Option<Sent> = None;
    let mut grounds = Grounds { displays: Vec::new(), timer: Timer::new(Instant::now()), hashes: HashMap::new() };
    loop {
        let message = match schedule.wait(Instant::now()) {
            Some(timeout) => inbox.recv_timeout(timeout),
            None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(message) => grounds.on(message, &mut schedule, Instant::now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        for message in inbox.try_iter() {
            grounds.on(message, &mut schedule, Instant::now());
        }
        if !schedule.take(Instant::now()) {
            continue;
        }
        grounds.picture(paused, &*sinks.ground);
        let may_capture = || schedule.may_capture(paused.load(Ordering::Relaxed), Instant::now());
        let Some((mut extras, stamps)) = picture(may_capture) else {
            continue;
        };
        if !extras::changed(last.as_ref().map(|sent| sent.stamps.as_slice()), &stamps) {
            continue;
        }
        if let Some(sent) = &last {
            for (item, from) in extras.items.iter_mut().zip(extras::reused(&sent.stamps, &stamps)) {
                if let Some(index) = from {
                    *item = sent.items[index].clone();
                }
            }
        }
        last = Some(Sent { stamps, items: extras.items.clone() });
        (sinks.extras)(extras);
    }
}

/// The displays whose grounds are pictured, when next, and what each last looked like.
struct Grounds {
    displays: Vec<(String, CGRect)>,
    timer: Timer,
    hashes: HashMap<String, u64>,
}

impl Grounds {
    fn on(&mut self, message: Message, schedule: &mut Schedule, now: Instant) {
        match message {
            Message::Extras(command) => {
                if command == Command::Refresh {
                    self.timer.now(now);
                }
                schedule.on(command, now);
            }
            Message::Grounds(displays) => {
                self.displays = displays;
                self.timer.now(now);
            }
        }
    }

    /// Pictures every display's ground if one is due, and sends the ones that changed. A pause that
    /// lands meanwhile stops it, and the rest stay due.
    fn picture(&mut self, paused: &AtomicBool, send: &dyn Fn(Ground)) {
        if !self.timer.take(Instant::now()) {
            return;
        }
        for (display, bounds) in &self.displays {
            if paused.load(Ordering::Relaxed) {
                self.timer.now(Instant::now());
                return;
            }
            let Some((picture, scale, hash)) = picture_ground(*bounds) else {
                continue;
            };
            if self.hashes.insert(display.clone(), hash) != Some(hash) {
                send(Ground { display: display.clone(), picture, scale });
            }
        }
    }
}

/// Where a status window is, beside what `select` reads of it.
#[derive(Clone, Copy)]
struct Placed {
    window: u32,
    bounds: CGRect,
}

/// Every extra drawn and what it is compared by. `None` when the capture failed, or `may_capture`
/// called it off after the listing.
fn picture(may_capture: impl FnOnce() -> bool) -> Option<(Extras, Vec<Stamp>)> {
    let (windows, placed) = status_windows();
    let chosen: Vec<(Placed, Kind)> =
        extras::select(&windows).into_iter().map(|(index, kind)| (placed[index], kind)).collect();
    if chosen.is_empty() {
        return Some((Extras::default(), Vec::new()));
    }
    let ids: Vec<u32> = chosen.iter().map(|(placed, _)| placed.window).collect();
    let bounds: Vec<CGRect> = chosen.iter().map(|(placed, _)| placed.bounds).collect();
    if !may_capture() {
        return None;
    }
    let image = capture(&ids, NULL_RECT)?;
    let composite = Composite::new(&bounds, (CGImage::width(Some(&image)), CGImage::height(Some(&image))))?;
    cut(&image, composite, &chosen)
}

/// The status-level windows on the active menu bar, and where each is. The main display's when which
/// menu bar is active cannot be read.
fn status_windows() -> (Vec<StatusWindow>, Vec<Placed>) {
    let display = CGDisplayBounds(active_menu_bar_display_id().unwrap_or_else(|| CGMainDisplayID()));
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
                y: bounds.origin.y,
                width: bounds.size.width,
                height: bounds.size.height,
            };
            Some((status, Placed { window: id, bounds }))
        })
        .filter(|(status, _)| extras::on_display(status, display))
        .unzip()
}

/// CGRectNull, which the bindings leave out: the union of the windows captured.
const NULL_RECT: CGRect = CGRect::new(CGPoint::new(f64::INFINITY, f64::INFINITY), CGSize::ZERO);

/// One picture of every window in `ids`, over `rect` in global coordinates or, with `NULL_RECT`, the
/// union of their bounds.
pub(super) fn capture(ids: &[u32], rect: CGRect) -> Option<CFRetained<CGImage>> {
    let mut image: *mut CGImage = std::ptr::null_mut();
    // SAFETY: `ids` outlives the call and holds `ids.len()` window ids; the image comes back retained.
    let err = unsafe {
        SLSCaptureWindowsContentsToRectWithOptions(
            *G_CONNECTION,
            ids.as_ptr(),
            ids.len() as i32,
            rect,
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
