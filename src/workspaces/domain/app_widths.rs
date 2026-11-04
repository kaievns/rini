//! The width the user last gave each application on each display shape, carried to the
//! application's new windows wherever that shape is attached. See
//! `src/workspaces/docs/launch-memory.md`, "An application's width".
//!
//! Keyed by bundle id and `DisplayShape`, not by window or arrangement: a window's own records die
//! with it and stay in their arrangement (`display_setup.rs`), while this outlives both, so a new
//! window of a running app, or an app opened at a desk not seen before, opens at the width the user
//! last chose for it on a display like this one.

use rustc_hash::FxHashMap as HashMap;
use serde::{Deserialize, Serialize};

use crate::displays::domain::screen::DisplayShape;
use crate::layout::ColumnWidth;

/// How many display shapes each application keeps a width for, the most recently set first. The
/// sizing research found 3 shapes among Kai's desks on 2026-11-04, so 8 keeps every desk and a few
/// rooms besides.
pub const APP_WIDTHS_PER_APP: usize = 8;

/// A width as a fraction of the display's usable width, absolute rather than an offset from
/// `default-column-width`, so a different default on another display reads it as the same width.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AppWidth {
    /// Full width, the mode `Mod+F` enters.
    FullWidth,
    /// Held to `min-column-width` and `max-column-width`, as a key's width is.
    Bounded(f64),
    /// Held only to the viewport, as a drag or the preset cycle's 100% gives it.
    Unbounded(f64),
}

impl AppWidth {
    /// `width`, an offset from `default`, as a fraction of the display.
    pub fn of(width: ColumnWidth, default: f64) -> Self {
        match width {
            ColumnWidth::FullWidth => Self::FullWidth,
            ColumnWidth::Offset(offset) => Self::Bounded(default + offset),
            ColumnWidth::Unbounded(offset) => Self::Unbounded(default + offset),
        }
    }

    fn is_finite(self) -> bool {
        match self {
            Self::FullWidth => true,
            Self::Bounded(fraction) | Self::Unbounded(fraction) => fraction.is_finite(),
        }
    }

    /// The width a column under `default` is given for it.
    pub fn column_width(self, default: f64) -> ColumnWidth {
        match self {
            Self::FullWidth => ColumnWidth::FullWidth,
            Self::Bounded(fraction) => ColumnWidth::Offset(fraction - default),
            Self::Unbounded(fraction) => ColumnWidth::Unbounded(fraction - default),
        }
    }
}

/// The default a remembered width overrode. A width made under another value is not the user's
/// answer to this one: an edit to the default MUST show on new windows, not be beaten by a width
/// chosen against the old one (`specs/displays.md`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Overrode {
    /// `layout › default-column-width`, as a fraction.
    DefaultColumnWidth(f64),
}

impl Overrode {
    /// Whether `other` is the same default. A fraction read back from the file is the one written,
    /// so a margin only absorbs arithmetic, never a real edit.
    fn same(self, other: Self) -> bool {
        match (self, other) {
            (Self::DefaultColumnWidth(a), Self::DefaultColumnWidth(b)) => (a - b).abs() < 1e-9,
        }
    }
}

/// One application's width on one display shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppWidthRecord {
    pub shape: DisplayShape,
    pub width: AppWidth,
    pub overrode: Overrode,
}

/// Every application's widths, by bundle id, each the most recently set first.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AppWidths {
    #[serde(default)]
    apps: HashMap<String, Vec<AppWidthRecord>>,
}

impl AppWidths {
    /// Records that the user gave `app` `width` on `shape` under the default `overrode`, `None`
    /// for back at the default, which forgets the shape's width. Drops what `app` remembers under
    /// any other default, and keeps the `APP_WIDTHS_PER_APP` most recent.
    pub fn remember(
        &mut self,
        app: &str,
        shape: DisplayShape,
        width: Option<AppWidth>,
        overrode: Overrode,
    ) {
        let records = self.apps.entry(app.to_owned()).or_default();
        records.retain(|record| record.shape != shape && record.overrode.same(overrode));
        if let Some(width) = width.filter(|width| width.is_finite()) {
            records.insert(0, AppWidthRecord { shape, width, overrode });
            records.truncate(APP_WIDTHS_PER_APP);
        }
        if records.is_empty() {
            self.apps.remove(app);
        }
    }

    /// The width `app` was last given on `shape`, when it was given under the default `under`.
    pub fn width(&self, app: &str, shape: DisplayShape, under: Overrode) -> Option<AppWidth> {
        self.apps
            .get(app)?
            .iter()
            .find(|record| record.shape == shape && record.overrode.same(under))
            .map(|record| record.width)
    }

    /// Drops every width made under a default other than `current`, and says how many.
    pub fn forget_made_under_another(&mut self, current: Overrode) -> usize {
        let mut dropped = 0;
        for records in self.apps.values_mut() {
            let before = records.len();
            records.retain(|record| record.overrode.same(current));
            dropped += before - records.len();
        }
        self.apps.retain(|_, records| !records.is_empty());
        dropped
    }

    pub fn is_empty(&self) -> bool { self.apps.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF: Overrode = Overrode::DefaultColumnWidth(0.5);

    fn shape(width_pt: u32) -> DisplayShape {
        DisplayShape {
            width_pt,
            height_pt: 1085,
            width_cm: 34,
        }
    }

    #[test]
    fn an_app_s_width_is_the_last_given_on_that_shape() {
        let mut widths = AppWidths::default();
        widths.remember("app", shape(1728), Some(AppWidth::Bounded(0.66667)), HALF);
        widths.remember("app", shape(1728), Some(AppWidth::FullWidth), HALF);
        widths.remember("app", shape(3008), Some(AppWidth::Bounded(0.33333)), HALF);
        assert_eq!(widths.width("app", shape(1728), HALF), Some(AppWidth::FullWidth));
        assert_eq!(
            widths.width("app", shape(3008), HALF),
            Some(AppWidth::Bounded(0.33333))
        );
        assert_eq!(widths.width("app", shape(1512), HALF), None);
        assert_eq!(widths.width("other", shape(1728), HALF), None);
    }

    /// Back at the default, the app's last width on that shape is the default.
    #[test]
    fn a_width_put_back_at_the_default_forgets_the_shape_s_width() {
        let mut widths = AppWidths::default();
        widths.remember("app", shape(1728), Some(AppWidth::FullWidth), HALF);
        widths.remember("app", shape(1728), None, HALF);
        assert_eq!(widths.width("app", shape(1728), HALF), None);
        assert!(widths.is_empty(), "an app with nothing left is dropped");
    }

    #[test]
    fn each_app_keeps_its_most_recent_shapes_up_to_the_cap() {
        let mut widths = AppWidths::default();
        let count = APP_WIDTHS_PER_APP as u32 + 2;
        for width_pt in 0..count {
            widths.remember("app", shape(1000 + width_pt), Some(AppWidth::FullWidth), HALF);
        }
        let kept: Vec<_> = (0..count)
            .filter(|width_pt| widths.width("app", shape(1000 + width_pt), HALF).is_some())
            .collect();
        assert_eq!(kept, (2..count).collect::<Vec<_>>());
    }

    #[test]
    fn a_width_made_under_another_default_is_not_read_and_is_dropped() {
        let mut widths = AppWidths::default();
        widths.remember("app", shape(1728), Some(AppWidth::FullWidth), HALF);
        widths.remember("other", shape(1728), Some(AppWidth::FullWidth), HALF);
        let edited = Overrode::DefaultColumnWidth(0.66667);
        assert_eq!(widths.width("app", shape(1728), edited), None);

        widths.remember("app", shape(3008), Some(AppWidth::Bounded(0.25)), edited);
        assert_eq!(
            widths.width("app", shape(1728), HALF),
            None,
            "a new width drops the app's widths under the old default"
        );
        assert_eq!(widths.forget_made_under_another(edited), 1);
        assert_eq!(widths.width("other", shape(1728), HALF), None);
        assert_eq!(
            widths.width("app", shape(3008), edited),
            Some(AppWidth::Bounded(0.25))
        );
    }

    /// The fraction stored is the width on screen: an offset from one default read under another
    /// is the same fraction of the display.
    #[test]
    fn a_width_is_stored_as_a_fraction_of_the_display_not_an_offset() {
        let cases = [
            (ColumnWidth::Offset(0.25), AppWidth::Bounded(0.75)),
            (ColumnWidth::Unbounded(-0.125), AppWidth::Unbounded(0.375)),
            (ColumnWidth::FullWidth, AppWidth::FullWidth),
        ];
        for (offset, fraction) in cases {
            let stored = AppWidth::of(offset, 0.5);
            assert_eq!(stored, fraction, "{offset:?}");
            let read = stored.column_width(0.625);
            assert_eq!(AppWidth::of(read, 0.625), fraction, "{offset:?}");
        }
    }
}
