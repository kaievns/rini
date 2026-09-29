//! Which of macOS's menu-bar extras the bar draws, where, and how much of each picture is icon.
//!
//! The bar draws the system's own status items rather than glyphs of its own: the Wi-Fi fan, the
//! speaker and the battery are Control Center's, and third-party extras are their apps' own icons.
//! Every one is hosted in the Control Center process as a window at the status level, named by its
//! module (`WiFi`, `Battery`) or anonymously (`Item-0`). See `src/bar/docs/menu-extras.md`.

/// The three extras drawn as vitals, beside the clock, rather than in the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vital {
    WiFi,
    Sound,
    Battery,
}

impl Vital {
    /// Left to right, as drawn.
    pub const ORDER: [Vital; 3] = [Vital::WiFi, Vital::Sound, Vital::Battery];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Vital(Vital),
    Tray,
    Skip,
}

/// The process every real menu extra lives in. An entry from any other owner is not one: after a
/// display replug the enumeration carried `LinkedNotesUIService`, a 268pt service window.
pub const HOST: &str = "Control Center";

/// Modules the bar already draws another way, or that a key reaches: the clock is drawn as text, and
/// Control Center's own button opens what the vitals show.
const SKIP: [&str; 7] = [
    "Clock",
    "BentoBox-0",
    "Now Playing",
    "NowPlaying",
    "PharosSystems.SecurePrint.JobManagement",
    "com.displaylink.DisplayLinkUserAgent",
    "",
];

/// Widest extra drawn, in points. A sanity guard, not a filter on text extras, which are wanted:
/// Outlook draws its next event as text, measured at 141pt and 170pt.
pub const MAX_WIDTH: f64 = 300.0;

/// A status-level window as the window server lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusWindow {
    pub owner: String,
    /// `kCGWindowName`: the module, a bundle id, or `Item-0`.
    pub name: String,
    /// Left edge on the menu bar, which is the order the extras are drawn in.
    pub x: f64,
    pub width: f64,
}

pub fn kind(window: &StatusWindow) -> Kind {
    if window.owner != HOST || window.width > MAX_WIDTH {
        return Kind::Skip;
    }
    match window.name.as_str() {
        "WiFi" => Kind::Vital(Vital::WiFi),
        "Sound" => Kind::Vital(Vital::Sound),
        "Battery" => Kind::Vital(Vital::Battery),
        name if SKIP.contains(&name) || name.starts_with("com.apple.") => Kind::Skip,
        _ => Kind::Tray,
    }
}

fn is_anonymous(name: &str) -> bool {
    name.strip_prefix("Item-").is_some_and(|rest| rest.parse::<u32>().is_ok())
}

fn is_bundle_id(name: &str) -> bool {
    name.contains('.')
}

/// The extras to draw, in menu-bar order left to right, each with its index in `windows`.
///
/// macOS can list the same third-party extras twice, once by bundle id and once as `Item-0`: 28
/// entries on one session, nine named and the same nine again anonymously. Only one block is kept,
/// and it is the anonymous one, because a capture by bundle id came back blank for Docker and
/// 1Password where the anonymous twin drew. The larger block wins if they ever differ, since losing
/// icons is the worse failure.
pub fn select(windows: &[StatusWindow]) -> Vec<(usize, Kind)> {
    let tray = |window: &&StatusWindow| kind(window) == Kind::Tray;
    let anonymous = windows.iter().filter(tray).filter(|w| is_anonymous(&w.name)).count();
    let named = windows.iter().filter(tray).filter(|w| is_bundle_id(&w.name)).count();
    let drop_named = anonymous > 0 && anonymous >= named;
    let drop_anonymous = !drop_named && named > 0;

    let mut out: Vec<(usize, Kind)> = windows
        .iter()
        .enumerate()
        .filter_map(|(index, window)| {
            let kind = kind(window);
            let duplicate = kind == Kind::Tray
                && ((drop_named && is_bundle_id(&window.name))
                    || (drop_anonymous && is_anonymous(&window.name)));
            (kind != Kind::Skip && !duplicate).then_some((index, kind))
        })
        .collect();
    out.sort_by(|a, b| windows[a.0].x.total_cmp(&windows[b.0].x));
    out
}

/// Opacity below which a pixel is not ink. The pictures have a transparent ground, and an icon's
/// anti-aliased edge fades well below this.
pub const INK_ALPHA: u8 = 24;

/// The first and last column holding ink, from each column's greatest alpha. `None` for a picture
/// with nothing in it.
///
/// This is what spaces the icons evenly. An extra's picture is its icon plus a margin, and the margin
/// is not the same from one extra to the next: 25.5pt to 39pt measured, and Apple's audio/video pill
/// carries 39.5pt of ink in a 62pt picture. Spaced by picture the row is uneven; spaced by ink it is
/// even.
pub fn ink_columns(column_alpha: &[u8]) -> Option<(usize, usize)> {
    let first = column_alpha.iter().position(|&alpha| alpha > INK_ALPHA)?;
    let last = column_alpha.iter().rposition(|&alpha| alpha > INK_ALPHA)?;
    Some((first, last))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(name: &str, x: f64) -> StatusWindow {
        StatusWindow { owner: HOST.into(), name: name.into(), x, width: 38.0 }
    }

    fn names(windows: &[StatusWindow]) -> Vec<(&str, Kind)> {
        select(windows).into_iter().map(|(index, kind)| (windows[index].name.as_str(), kind)).collect()
    }

    /// The enumeration this machine gave, right to left as the window server lists it.
    fn session() -> Vec<StatusWindow> {
        vec![
            status("Clock", 1680.0),
            status("BentoBox-0", 1640.0),
            status("Sound", 1600.0),
            status("WiFi", 1560.0),
            status("Battery", 1520.0),
            status("Item-0", 1380.0),
            status("Item-0", 1346.0),
            status("AudioVideoModule", 1100.0),
        ]
    }

    #[test]
    fn the_vitals_are_picked_out_by_name() {
        assert_eq!(kind(&status("WiFi", 0.0)), Kind::Vital(Vital::WiFi));
        assert_eq!(kind(&status("Sound", 0.0)), Kind::Vital(Vital::Sound));
        assert_eq!(kind(&status("Battery", 0.0)), Kind::Vital(Vital::Battery));
    }

    /// The clock is drawn as text and Control Center's own button duplicates the vitals.
    #[test]
    fn what_the_bar_draws_another_way_is_skipped() {
        assert_eq!(kind(&status("Clock", 0.0)), Kind::Skip);
        assert_eq!(kind(&status("BentoBox-0", 0.0)), Kind::Skip);
        assert_eq!(kind(&status("com.apple.Spotlight", 0.0)), Kind::Skip);
    }

    /// Only Control Center hosts real extras.
    #[test]
    fn a_window_from_another_owner_is_not_an_extra() {
        let mut window = status("Window(4)", 0.0);
        window.owner = "LinkedNotesUIService".into();
        assert_eq!(kind(&window), Kind::Skip);
    }

    /// Outlook's event text is an extra and is drawn; something 300pt wide is not one.
    #[test]
    fn a_text_extra_is_kept_and_a_huge_one_is_not() {
        let mut outlook = status("Item-0", 0.0);
        outlook.width = 170.0;
        assert_eq!(kind(&outlook), Kind::Tray);
        outlook.width = 320.0;
        assert_eq!(kind(&outlook), Kind::Skip);
    }

    /// Drawn left to right, as the menu bar has them.
    #[test]
    fn extras_come_out_in_menu_bar_order() {
        assert_eq!(
            names(&session()),
            vec![
                ("AudioVideoModule", Kind::Tray),
                ("Item-0", Kind::Tray),
                ("Item-0", Kind::Tray),
                ("Battery", Kind::Vital(Vital::Battery)),
                ("WiFi", Kind::Vital(Vital::WiFi)),
                ("Sound", Kind::Vital(Vital::Sound)),
            ]
        );
    }

    /// With both blocks listed, the anonymous one is kept: it is the one whose captures draw.
    #[test]
    fn the_anonymous_twin_block_wins() {
        let windows = vec![
            status("com.electron.dockerdesktop", 100.0),
            status("com.agilebits.onepassword7", 140.0),
            status("Item-0", 200.0),
            status("Item-0", 240.0),
        ];
        let kept: Vec<f64> = select(&windows).iter().map(|(index, _)| windows[*index].x).collect();
        assert_eq!(kept, vec![200.0, 240.0]);
    }

    /// If one block is short, the bigger one is kept.
    #[test]
    fn the_larger_block_wins_a_mismatch() {
        let windows = vec![
            status("com.microsoft.Outlook", 100.0),
            status("us.zoom.xos", 140.0),
            status("Item-0", 200.0),
        ];
        let kept: Vec<&str> =
            select(&windows).iter().map(|(index, _)| windows[*index].name.as_str()).collect();
        assert_eq!(kept, vec!["com.microsoft.Outlook", "us.zoom.xos"]);
    }

    /// Named extras alone are all kept: some sessions resolve every name.
    #[test]
    fn a_session_of_named_extras_keeps_them() {
        let windows = vec![status("com.amazon.ACME", 100.0), status("us.zoom.xos", 140.0)];
        assert_eq!(select(&windows).len(), 2);
    }

    #[test]
    fn ink_is_the_span_of_opaque_columns() {
        assert_eq!(ink_columns(&[0, 0, 30, 255, 0, 200, 10, 0]), Some((2, 5)));
        assert_eq!(ink_columns(&[255]), Some((0, 0)));
    }

    /// A faint edge is not ink, and a blank picture has none.
    #[test]
    fn a_blank_picture_has_no_ink() {
        assert_eq!(ink_columns(&[0, INK_ALPHA, 3, 0]), None);
        assert_eq!(ink_columns(&[]), None);
    }
}
