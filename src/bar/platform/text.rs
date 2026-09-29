//! Setting the bar's words: fonts, ink measurement, and the pictures the layers show.
//!
//! Each string is measured by its ink and drawn once into a picture of that ink, kept for as long
//! as some bar still says it. See "Drawing" in `src/bar/docs/README.md`.

use std::collections::HashSet;
use std::hash::Hash;
use std::rc::Rc;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAttributedStringNSExtendedStringDrawing, NSColor, NSFont, NSFontAttributeName, NSFontWeight,
    NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold, NSForegroundColorAttributeName,
    NSGraphicsContext, NSStringDrawingOptions,
};
use objc2_core_foundation::{CFRetained, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage, kCGColorSpaceSRGB,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSString};
use rustc_hash::FxHashMap as HashMap;
use tracing::warn;

use crate::animation::platform::edge_dressing::rgba_bitmap_context;
use crate::bar::domain::pieces::Label;
use crate::bar::domain::placement::{Ink, Sheet, sheet};

/// A string drawn around its ink.
pub struct Picture {
    pub image: CFRetained<CGImage>,
    pub sheet: Sheet,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    face: &'static str,
    size: u64,
    colour: u32,
    stand_in: Option<String>,
}

impl Key {
    fn of(label: &Label) -> Self {
        Key {
            text: label.text.clone(),
            face: label.face.name,
            size: label.face.size.to_bits(),
            colour: label.colour.0,
            stand_in: label.stand_in.clone(),
        }
    }
}

/// Every string's picture, drawn once.
pub struct Text {
    scale: f64,
    /// `None` for a string with no ink, so it is not measured again.
    pictures: Recent<Key, Option<Rc<Picture>>>,
    /// Faces already warned about.
    missing: HashSet<&'static str>,
}

impl Text {
    pub fn new(scale: f64) -> Self {
        Text {
            scale,
            pictures: Recent::default(),
            missing: HashSet::new(),
        }
    }

    pub fn picture(&mut self, label: &Label) -> Option<Rc<Picture>> {
        let (scale, missing) = (self.scale, &mut self.missing);
        self.pictures
            .get_or_make(Key::of(label), || draw(label, scale, missing).map(Rc::new))
    }

    /// Called once per round of drawing every bar, so a title that has gone is not kept for the
    /// life of the process.
    pub fn sweep(&mut self) {
        self.pictures.sweep();
    }
}

/// Keeps what was asked for this round or the one before, and nothing older.
struct Recent<K, V> {
    current: HashMap<K, V>,
    previous: HashMap<K, V>,
}

impl<K, V> Default for Recent<K, V> {
    fn default() -> Self {
        Recent {
            current: HashMap::default(),
            previous: HashMap::default(),
        }
    }
}

impl<K: Eq + Hash, V: Clone> Recent<K, V> {
    fn get_or_make(&mut self, key: K, make: impl FnOnce() -> V) -> V {
        if let Some(value) = self.current.get(&key) {
            return value.clone();
        }
        let value = self.previous.remove(&key).unwrap_or_else(make);
        self.current.insert(key, value.clone());
        value
    }

    fn sweep(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }
}

fn draw(label: &Label, scale: f64, missing: &mut HashSet<&'static str>) -> Option<Picture> {
    let named = NSFont::fontWithName_size(&NSString::from_str(label.face.name), label.face.size);
    let (font, text) = match named {
        Some(font) => (font, label.text.as_str()),
        None => {
            if missing.insert(label.face.name) {
                warn!(
                    face = label.face.name,
                    "font not installed; the bar sets it in the system's monospaced face"
                );
            }
            let weight = ns_weight(weight(label.face.name));
            let font = NSFont::monospacedSystemFontOfSize_weight(label.face.size, weight);
            (font, label.stand_in.as_deref().unwrap_or(&label.text))
        }
    };
    let (red, green, blue) = label.colour.rgb();
    let colour = NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, 1.0);
    let string = attributed(text, &font, &colour);
    // Without UsesLineFragmentOrigin the rect is from the pen on the baseline, y up.
    let ink = string.boundingRectWithSize_options_context(
        CGSize::new(10_000.0, 10_000.0),
        NSStringDrawingOptions::UsesDeviceMetrics,
        None,
    );
    if ink.size.width <= 0.0 || ink.size.height <= 0.0 {
        return None;
    }
    let sheet = sheet(
        Ink {
            x: ink.origin.x,
            y: ink.origin.y,
            width: ink.size.width,
            height: ink.size.height,
        },
        scale,
    );
    let image = render(&string, &sheet, scale)?;
    Some(Picture { image, sheet })
}

fn attributed(text: &str, font: &NSFont, colour: &NSColor) -> Retained<NSAttributedString> {
    // SAFETY: AppKit's own attribute names, immutable statics.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [font.as_ref(), colour.as_ref()];
    let attributes = NSDictionary::from_slices(&keys, &values);
    // SAFETY: each attribute carries the type AppKit expects for it, an NSFont and an NSColor.
    unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(text),
            Some(&attributes),
        )
    }
}

/// `string` drawn from `sheet`'s pen, into a picture the size of the sheet.
///
/// The bitmap is device RGB, which takes the sRGB palette's bytes unchanged (measured 2026-09-29);
/// the picture is then tagged sRGB so it is matched to the display as the bar's ground colour is.
fn render(string: &NSAttributedString, sheet: &Sheet, scale: f64) -> Option<CFRetained<CGImage>> {
    let width = (sheet.size.width * scale).round() as usize;
    let height = (sheet.size.height * scale).round() as usize;
    let context = rgba_bitmap_context(width, height)?;
    CGContext::translate_ctm(Some(&context), 0.0, height as f64);
    CGContext::scale_ctm(Some(&context), scale, -scale);
    let graphics = NSGraphicsContext::graphicsContextWithCGContext_flipped(&context, true);
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&graphics));
    string.drawWithRect_options_context(
        CGRect::new(sheet.pen, CGSize::new(0.0, 0.0)),
        NSStringDrawingOptions::empty(),
        None,
    );
    NSGraphicsContext::restoreGraphicsState_class();
    let drawn = CGBitmapContextCreateImage(Some(&context))?;
    // SAFETY: a CoreGraphics constant, an immutable static.
    let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    CGImage::new_copy_with_color_space(Some(&drawn), Some(&srgb))
}

/// The stand-in face's weight, read off the missing face's PostScript name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Weight {
    Regular,
    Medium,
    Semibold,
}

fn weight(face: &str) -> Weight {
    if face.ends_with("-SemiBold") {
        Weight::Semibold
    } else if face.ends_with("-Medium") {
        Weight::Medium
    } else {
        Weight::Regular
    }
}

fn ns_weight(weight: Weight) -> NSFontWeight {
    // SAFETY: AppKit's weight constants, immutable statics.
    unsafe {
        match weight {
            Weight::Regular => NSFontWeightRegular,
            Weight::Medium => NSFontWeightMedium,
            Weight::Semibold => NSFontWeightSemibold,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::bar::domain::layout::Piece;
    use crate::bar::domain::model::{DisplayBar, Row};
    use crate::bar::domain::style;

    /// A missing face is stood in for at the weight its name asks for.
    #[test]
    fn the_stand_in_keeps_the_weight() {
        let bar = DisplayBar {
            uuid: "d".into(),
            screen: 1,
            frame: CGRect::ZERO,
            rows: vec![Row::Shown, Row::Occupied],
            glyphs: vec![],
            focus: None,
        };
        assert_eq!(
            weight(style::face(Piece::Numeral(0), &bar).name),
            Weight::Semibold
        );
        assert_eq!(weight(style::face(Piece::Numeral(1), &bar).name), Weight::Medium);
        assert_eq!(weight(style::face(Piece::Date, &bar).name), Weight::Regular);
        assert_eq!(weight(style::face(Piece::Glyph(0), &bar).name), Weight::Regular);
    }

    /// Drawn once while it is still being said, and dropped after a whole round without it.
    #[test]
    fn a_picture_lasts_as_long_as_it_is_asked_for() {
        let made = Cell::new(0);
        let ask = |recent: &mut Recent<&str, usize>, key| {
            recent.get_or_make(key, || {
                made.set(made.get() + 1);
                made.get()
            })
        };
        let mut recent = Recent::default();
        assert_eq!(ask(&mut recent, "12:04"), 1);
        assert_eq!(ask(&mut recent, "12:04"), 1);
        recent.sweep();
        assert_eq!(ask(&mut recent, "12:04"), 1, "kept across one sweep");
        assert_eq!(ask(&mut recent, "12:05"), 2);
        recent.sweep();
        recent.sweep();
        assert_eq!(ask(&mut recent, "12:04"), 3, "gone after a round without it");
    }
}
