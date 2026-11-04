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
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
struct AppWidthRecord {
    shape: DisplayShape,
    width: AppWidth,
    overrode: Overrode,
}

/// Every application's widths, by bundle id, each the most recently set first.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(from = "StoredAppWidths", into = "StoredAppWidths")]
pub struct AppWidths {
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

    pub fn is_empty(&self) -> bool { self.apps.is_empty() }
}

/// `AppWidths` as a file holds it: structs of optional fields and no enum, so a field a later rini
/// adds is skipped by this one, and a record this one cannot read is dropped alone rather than
/// refusing the file with every home and slot in it (`specs/displays.md`). A later kind of width or
/// of default leaves the field this rini reads empty, so this rini drops that record rather than
/// misreading it.
#[derive(Default, Serialize, Deserialize)]
struct StoredAppWidths {
    #[serde(default)]
    apps: HashMap<String, Vec<StoredRecord>>,
}

#[derive(Serialize, Deserialize)]
struct StoredRecord {
    #[serde(default)]
    shape: Option<DisplayShape>,
    #[serde(default)]
    width: StoredWidth,
    #[serde(default)]
    overrode: StoredOverrode,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredWidth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fraction: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unbounded: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    full: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredOverrode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_column_width: Option<f64>,
}

impl StoredWidth {
    fn of(width: AppWidth) -> Self {
        match width {
            AppWidth::FullWidth => Self { full: true, ..Self::default() },
            AppWidth::Bounded(fraction) => Self {
                fraction: Some(fraction),
                ..Self::default()
            },
            AppWidth::Unbounded(fraction) => Self {
                fraction: Some(fraction),
                unbounded: true,
                ..Self::default()
            },
        }
    }

    fn read(&self) -> Option<AppWidth> {
        if self.full {
            return Some(AppWidth::FullWidth);
        }
        let fraction = self.fraction.filter(|fraction| fraction.is_finite())?;
        Some(if self.unbounded {
            AppWidth::Unbounded(fraction)
        } else {
            AppWidth::Bounded(fraction)
        })
    }
}

impl StoredRecord {
    fn read(&self) -> Option<AppWidthRecord> {
        Some(AppWidthRecord {
            shape: self.shape?,
            width: self.width.read()?,
            overrode: Overrode::DefaultColumnWidth(self.overrode.default_column_width?),
        })
    }
}

impl From<StoredAppWidths> for AppWidths {
    fn from(stored: StoredAppWidths) -> Self {
        let apps = stored
            .apps
            .into_iter()
            .filter_map(|(app, records)| {
                let mut records: Vec<_> = records.iter().filter_map(StoredRecord::read).collect();
                records.truncate(APP_WIDTHS_PER_APP);
                (!records.is_empty()).then_some((app, records))
            })
            .collect();
        Self { apps }
    }
}

impl From<AppWidths> for StoredAppWidths {
    fn from(widths: AppWidths) -> Self {
        let apps = widths
            .apps
            .into_iter()
            .map(|(app, records)| {
                let records = records
                    .into_iter()
                    .map(|record| {
                        let Overrode::DefaultColumnWidth(default) = record.overrode;
                        StoredRecord {
                            shape: Some(record.shape),
                            width: StoredWidth::of(record.width),
                            overrode: StoredOverrode {
                                default_column_width: Some(default),
                            },
                        }
                    })
                    .collect();
                (app, records)
            })
            .collect();
        Self { apps }
    }
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
    fn a_width_made_under_another_default_is_not_read_and_is_dropped_at_its_app_s_next_write() {
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
        assert_eq!(
            widths.width("other", shape(1728), HALF),
            Some(AppWidth::FullWidth),
            "another app's is kept until its own next write"
        );
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
