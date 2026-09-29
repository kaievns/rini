//! What each display's bar shows, decided from what the window manager knows.
//!
//! The reactor fills a `BarInput` from its own stores after every batch of events, this turns it into a
//! `BarModel`, and the bar is sent the model only when it differs from the last one. So the bar never
//! asks for anything, and a burst of events that changes nothing on it costs it nothing.

use objc2_core_foundation::CGRect;
use rini_core::ids::WindowId;

use super::format::truncate;

/// Longest window title drawn, in characters. Titles are unbounded (74 characters on one Obsidian
/// window), and an unbounded label would run into the right half of the bar.
pub const TITLE_MAX_CHARS: usize = 48;

/// Everything the bar is built from, as the reactor knows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarInput {
    pub displays: Vec<DisplayInput>,
    pub focus: Option<FocusInput>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInput {
    /// The display's durable identity. Space ids change on every reconnect; this does not.
    pub uuid: String,
    /// The CoreGraphics display id, which is what the bar's window is placed by.
    pub screen: u32,
    /// The display's frame, which changes with its resolution and arrangement.
    pub frame: CGRect,
    /// Whether each workspace holds a window on this display, in workspace order.
    pub occupied: Vec<bool>,
    /// Which workspace this display shows.
    pub shown: Option<usize>,
    /// The shown workspace's windows, in the order the workspace holds them.
    pub windows: Vec<WindowInput>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowInput {
    pub window: WindowId,
    /// The application's localised name. Empty when the window manager has none for it.
    pub app: String,
}

/// The focused window, and the display it is on.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusInput {
    pub display: String,
    pub window: WindowId,
    pub app: String,
    pub title: String,
}

/// What every bar shows. No displays means no bars.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarModel {
    pub displays: Vec<DisplayBar>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayBar {
    pub uuid: String,
    pub screen: u32,
    /// Never drawn. A display that is resized or moved makes a new model, and so is placed again.
    pub frame: CGRect,
    /// One per workspace, in workspace order.
    pub rows: Vec<Row>,
    /// The shown workspace's applications, the focused window's first.
    pub glyphs: Vec<Glyph>,
    /// Only on the display the focused window is on. The others stay silent rather than repeat it.
    pub focus: Option<FocusLabel>,
}

/// How a workspace's numeral is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// The workspace this display shows.
    Shown,
    /// Holds windows on this display.
    Occupied,
    Empty,
}

/// One application on the shown workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    pub app: String,
    /// What a click on it focuses: the focused window for the lit glyph, the application's first
    /// window on the workspace for the rest.
    pub window: WindowId,
    /// The focused window's application.
    pub lit: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FocusLabel {
    pub app: String,
    pub title: String,
}

/// What a click on the bar asks the window manager for.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Show workspace `index` on the display `display`, whichever display has focus.
    ShowWorkspace { display: String, index: usize },
    Focus(WindowId),
}

pub fn build(input: &BarInput) -> BarModel {
    BarModel {
        displays: input.displays.iter().map(|display| display_bar(display, input.focus.as_ref())).collect(),
    }
}

fn display_bar(display: &DisplayInput, focus: Option<&FocusInput>) -> DisplayBar {
    let rows = display
        .occupied
        .iter()
        .enumerate()
        .map(|(index, &occupied)| match (display.shown == Some(index), occupied) {
            (true, _) => Row::Shown,
            (false, true) => Row::Occupied,
            (false, false) => Row::Empty,
        })
        .collect();

    let focus_here = focus.filter(|focus| focus.display == display.uuid);
    DisplayBar {
        uuid: display.uuid.clone(),
        screen: display.screen,
        frame: display.frame,
        rows,
        glyphs: glyphs(&display.windows, focus_here.map(|focus| focus.window)),
        focus: focus_here.filter(|focus| !focus.app.is_empty()).map(|focus| FocusLabel {
            app: focus.app.clone(),
            title: truncate(&focus.title, TITLE_MAX_CHARS),
        }),
    }
}

/// Distinct applications, the focused window's first so it is never the one folded away.
fn glyphs(windows: &[WindowInput], focused: Option<WindowId>) -> Vec<Glyph> {
    let named = || windows.iter().filter(|window| !window.app.is_empty());
    let lit = focused.and_then(|focused| named().find(|window| window.window == focused));

    let mut out: Vec<Glyph> = lit
        .map(|window| Glyph { app: window.app.clone(), window: window.window, lit: true })
        .into_iter()
        .collect();
    for window in named() {
        if !out.iter().any(|glyph| glyph.app == window.app) {
            out.push(Glyph { app: window.app.clone(), window: window.window, lit: false });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    const BUILTIN: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    const EXTERNAL: &str = "B5E7ECDB-94D9-4565-949D-5F22F78D104A";

    fn window(idx: u32, app: &str) -> WindowInput {
        WindowInput { window: WindowId::new(800 + idx as i32, idx), app: app.to_string() }
    }

    /// The shapes of a real dump: the built-in shows workspace 4 with eight applications and Ghostty
    /// focused, the external shows an empty workspace 1.
    fn input() -> BarInput {
        BarInput {
            displays: vec![
                DisplayInput {
                    uuid: BUILTIN.into(),
                    screen: 1,
                    frame: CGRect::new(CGPoint::new(0.0, 32.0), CGSize::new(1728.0, 1085.0)),
                    occupied: vec![true, false, false, true],
                    shown: Some(3),
                    windows: vec![
                        window(1, "Alacritty"),
                        window(2, "Code"),
                        window(3, "Ghostty"),
                        window(4, "Ghostty"),
                        window(5, "Zen"),
                        window(6, ""),
                    ],
                },
                DisplayInput {
                    uuid: EXTERNAL.into(),
                    screen: 2,
                    frame: CGRect::new(CGPoint::new(-670.0, -1660.0), CGSize::new(3008.0, 1660.0)),
                    occupied: vec![false, false, false, false],
                    shown: Some(0),
                    windows: vec![],
                },
            ],
            focus: Some(FocusInput {
                display: BUILTIN.into(),
                window: WindowId::new(804, 4),
                app: "Ghostty".into(),
                title: "~/p/rini".into(),
            }),
        }
    }

    /// Each display draws its own rows: the one it shows, the ones holding windows ON THAT DISPLAY,
    /// and the empty ones. An earlier bar drew one shared set for both displays.
    #[test]
    fn each_display_has_its_own_rows() {
        let model = build(&input());
        assert_eq!(model.displays[0].rows, vec![Row::Occupied, Row::Empty, Row::Empty, Row::Shown]);
        assert_eq!(model.displays[1].rows, vec![Row::Shown, Row::Empty, Row::Empty, Row::Empty]);
    }

    /// The focused window's application comes first and is lit, whatever position its window has.
    /// Its glyph focuses the focused window itself, not the application's first one.
    #[test]
    fn the_focused_application_leads_the_glyphs() {
        let glyphs = &build(&input()).displays[0].glyphs;
        assert_eq!(glyphs[0], Glyph { app: "Ghostty".into(), window: WindowId::new(804, 4), lit: true });
        let apps: Vec<&str> = glyphs.iter().map(|glyph| glyph.app.as_str()).collect();
        assert_eq!(apps, vec!["Ghostty", "Alacritty", "Code", "Zen"]);
        assert!(glyphs[1..].iter().all(|glyph| !glyph.lit));
    }

    /// One glyph per application, standing for its first window, and a window with no application
    /// name gets none.
    #[test]
    fn an_application_is_one_glyph() {
        let mut input = input();
        input.focus = None;
        let glyphs = &build(&input).displays[0].glyphs;
        let apps: Vec<&str> = glyphs.iter().map(|glyph| glyph.app.as_str()).collect();
        assert_eq!(apps, vec!["Alacritty", "Code", "Ghostty", "Zen"]);
        assert_eq!(glyphs[2].window, WindowId::new(803, 3));
    }

    /// The focused window and its title go on the display it is on, and the other stays silent.
    #[test]
    fn the_focus_label_is_on_the_focused_display_only() {
        let model = build(&input());
        assert_eq!(
            model.displays[0].focus,
            Some(FocusLabel { app: "Ghostty".into(), title: "~/p/rini".into() })
        );
        assert_eq!(model.displays[1].focus, None);
    }

    /// A focused window that is not on the shown workspace lights nothing there.
    #[test]
    fn a_focused_window_elsewhere_lights_no_glyph() {
        let mut input = input();
        input.focus = Some(FocusInput {
            display: EXTERNAL.into(),
            window: WindowId::new(804, 4),
            app: "Ghostty".into(),
            title: String::new(),
        });
        let model = build(&input);
        assert!(model.displays[0].glyphs.iter().all(|glyph| !glyph.lit));
        assert_eq!(model.displays[0].focus, None);
    }

    /// With nothing focused the label goes away rather than holding the last application's name.
    #[test]
    fn no_focus_is_no_label() {
        let mut input = input();
        input.focus = None;
        assert!(build(&input).displays.iter().all(|display| display.focus.is_none()));
    }

    /// A window the window manager has no application name for has nothing to label.
    #[test]
    fn a_nameless_focused_application_has_no_label() {
        let mut input = input();
        input.focus.as_mut().unwrap().app = String::new();
        assert_eq!(build(&input).displays[0].focus, None);
    }

    #[test]
    fn a_long_title_is_cut() {
        let mut input = input();
        input.focus.as_mut().unwrap().title = "x".repeat(80);
        let title = build(&input).displays[0].focus.clone().unwrap().title;
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    /// Every workspace gets a numeral. The six-row cap the old bar had was its item budget, not a
    /// choice about the bar.
    #[test]
    fn every_workspace_has_a_row() {
        let mut input = input();
        input.displays[0].occupied = vec![false; 9];
        input.displays[0].shown = Some(8);
        let rows = &build(&input).displays[0].rows;
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[8], Row::Shown);
    }

    /// A display that changes resolution or moves, and nothing else, still makes a different model,
    /// so the change is sent and its bar is placed again.
    #[test]
    fn a_resized_display_is_a_different_model() {
        let before = build(&input());
        let mut input = input();
        input.displays[1].frame.size = CGSize::new(3840.0, 2128.0);
        let after = build(&input);
        assert_eq!(after.displays[1].frame, input.displays[1].frame);
        assert_ne!(after, before);
    }

    /// A model with nothing in it is what takes the bars down.
    #[test]
    fn no_displays_is_no_bars() {
        assert_eq!(build(&BarInput::default()), BarModel::default());
    }
}
