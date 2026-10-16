//! What a strip can be told about itself: gaps, column widths, how it follows focus.
//! Plain data with defaults; the config file fills it.
use std::collections::BTreeMap;

use rini_core::ids::DisplayUuid;

/// How the strip follows focus. niri's `center-focused-column`, without its `on-overflow`.
#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub enum CenterFocusedColumn {
    /// The strip moves only as far as it must to show the focused column.
    #[default]
    Never,
    /// The focused column sits in the middle, except the first and last, which sit flush.
    Always,
}

#[derive(Debug, PartialEq, Clone, Default)]
pub struct LayoutSettings {
    pub gaps: GapSettings,
    pub scrolling: ScrollingLayoutSettings,
}

#[derive(Debug, PartialEq, Clone)]
pub struct ScrollingLayoutSettings {
    pub column_width_ratio: f64,
    pub min_column_width_ratio: f64,
    pub max_column_width_ratio: f64,
    /// Column widths cycled by `switch-preset-column-width`, as ratios of the tiling width. niri's
    /// presets stop at two thirds; 1.0 is here too, the whole viewport held only to it rather than to
    /// `max_column_width_ratio` (`layout::domain::preset_width`), and never the maximise mode, so a
    /// maximised column can be stepped back down. Ratios outside `(0, 1]` are ignored.
    pub preset_column_widths: Vec<f64>,
    pub center_focused_column: CenterFocusedColumn,
}

impl Default for ScrollingLayoutSettings {
    fn default() -> Self {
        Self {
            column_width_ratio: 0.7,
            min_column_width_ratio: 0.3,
            max_column_width_ratio: 0.9,
            preset_column_widths: vec![0.33333, 0.5, 0.66667, 1.0],
            center_focused_column: CenterFocusedColumn::default(),
        }
    }
}

impl ScrollingLayoutSettings {
    /// Whether the strip is pinned and only moves to reveal the focused column.
    pub fn reveals_only(&self) -> bool { self.center_focused_column == CenterFocusedColumn::Never }
}

/// Space at each edge of a display, in logical pixels.
#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub struct Edges {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
}

/// The gaps as configured: niri's `gaps`, between windows and at the edges, and `struts`, extra
/// space at each edge on top of `gaps`. A display can replace either one as a whole.
#[derive(Debug, PartialEq, Clone, Default)]
pub struct GapSettings {
    pub gaps: f64,
    pub struts: Edges,
    /// By display UUID.
    pub per_display: BTreeMap<DisplayUuid, GapOverride>,
}

#[derive(Debug, PartialEq, Clone, Default)]
pub struct GapOverride {
    pub gaps: Option<f64>,
    pub struts: Option<Edges>,
}

/// The gaps one display lays out with.
#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub struct Gaps {
    /// Between the windows and the display's edges.
    pub outer: Edges,
    /// Between neighbouring windows, both ways.
    pub inner: f64,
}

impl GapSettings {
    pub fn effective_for_display(&self, display_uuid: Option<&DisplayUuid>) -> Gaps {
        let display = display_uuid.and_then(|uuid| self.per_display.get(uuid));
        let gaps = display.and_then(|d| d.gaps).unwrap_or(self.gaps);
        let struts = display.and_then(|d| d.struts).unwrap_or(self.struts);
        Gaps {
            outer: Edges {
                top: gaps + struts.top,
                left: gaps + struts.left,
                bottom: gaps + struts.bottom,
                right: gaps + struts.right,
            },
            inner: gaps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outer_gap_is_gaps_plus_the_strut_on_that_side() {
        let settings = GapSettings {
            gaps: 3.0,
            struts: Edges { top: 20.0, ..Edges::default() },
            per_display: BTreeMap::new(),
        };
        let gaps = settings.effective_for_display(None);
        assert_eq!(gaps.inner, 3.0);
        assert_eq!(gaps.outer, Edges {
            top: 23.0,
            left: 3.0,
            bottom: 3.0,
            right: 3.0
        });
    }

    /// niri's merge: a display's `struts` replaces the struts as a whole, so a side it leaves out is
    /// 0 there, while the top-level `gaps` still applies.
    #[test]
    fn a_display_replaces_gaps_and_struts_each_as_a_whole() {
        let mut settings = GapSettings {
            gaps: 3.0,
            struts: Edges {
                top: 5.0,
                left: 5.0,
                bottom: 5.0,
                right: 5.0,
            },
            per_display: BTreeMap::new(),
        };
        settings.per_display.insert("D".into(), GapOverride {
            gaps: None,
            struts: Some(Edges { top: 20.0, ..Edges::default() }),
        });
        settings
            .per_display
            .insert("G".into(), GapOverride { gaps: Some(8.0), struts: None });

        let struts_only = settings.effective_for_display(Some(&DisplayUuid::new("D")));
        assert_eq!(struts_only.inner, 3.0);
        assert_eq!(struts_only.outer, Edges {
            top: 23.0,
            left: 3.0,
            bottom: 3.0,
            right: 3.0
        });

        let gaps_only = settings.effective_for_display(Some(&DisplayUuid::new("G")));
        assert_eq!(gaps_only.inner, 8.0);
        assert_eq!(gaps_only.outer, Edges {
            top: 13.0,
            left: 13.0,
            bottom: 13.0,
            right: 13.0
        });

        assert_eq!(
            settings.effective_for_display(Some(&DisplayUuid::new("other"))),
            settings.effective_for_display(None)
        );
    }
}
