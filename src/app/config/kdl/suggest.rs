//! What a name rini does not know was probably meant to be.
//!
//! Two tables. `SCHEMA` is every node name each section takes, and a test decodes each entry, so it
//! cannot list a name the decoder refuses. `NIRI` is the names niri has and rini does not, each with
//! the reason, so a pasted niri snippet is told why rather than offered a lookalike.

use knus::ast::{Document, SpannedNode};
use knus::span::Span;
use strum::VariantNames;

use super::error::Problem;
use crate::input::domain::binding::Action;

/// Each section, by its path from the top, and the node names it takes. A section whose children are
/// free-form (`binds`, `icons`) is absent, and is not walked.
pub const SCHEMA: &[(&str, &[&str])] = &[
    ("", &[
        "input",
        "layout",
        "animations",
        "gestures",
        "recent-windows",
        "bar",
        "config-notification",
        "output",
        "workspace",
        "window-rule",
        "spawn-at-startup",
        "spawn-sh-at-startup",
        "binds",
        "activate-spaces-by-default",
        "reapply-window-rules-on-title-change",
    ]),
    ("input", &[
        "mod-key",
        "focus-follows-mouse",
        "warp-mouse-to-focus",
        "warp-mouse-across-stacked-outputs",
        "ignore-app-activation",
    ]),
    ("layout", &[
        "gaps",
        "struts",
        "default-column-width",
        "min-column-width",
        "max-column-width",
        "preset-column-widths",
        "center-focused-column",
        "focus-ring",
        "border",
    ]),
    ("layout/struts", &SIDES),
    ("layout/focus-ring", &BAND),
    ("layout/border", &BAND),
    ("layout/default-column-width", &["proportion"]),
    ("layout/min-column-width", &["proportion"]),
    ("layout/max-column-width", &["proportion"]),
    ("layout/preset-column-widths", &["proportion"]),
    ("animations", &["off", "slowdown", "journal"]),
    ("animations/journal", &["measure-handover"]),
    ("gestures", &["drag-swap-overlap"]),
    ("recent-windows", &["icons", "binds"]),
    ("bar", &["off"]),
    ("config-notification", &["disable-failed"]),
    ("output", &["layout"]),
    ("output/layout", &["gaps", "struts"]),
    ("output/layout/struts", &SIDES),
    ("workspace", &["focus-at-startup"]),
    ("window-rule", &[
        "match",
        "exclude",
        "open-on-workspace",
        "open-floating",
        "default-floating-position",
        "default-column-width",
        "default-window-height",
        "open-focused",
        "manage",
        "open-on-output",
        "focus-ring",
        "border",
        "geometry-corner-radius",
        "draw-border-with-background",
    ]),
    ("window-rule/focus-ring", &BAND),
    ("window-rule/border", &BAND),
    ("window-rule/default-column-width", &["fixed"]),
    ("window-rule/default-window-height", &["fixed"]),
];

const SIDES: [&str; 4] = ["left", "right", "top", "bottom"];
const BAND: [&str; 9] = [
    "on",
    "off",
    "width",
    "active-color",
    "inactive-color",
    "urgent-color",
    "active-gradient",
    "inactive-gradient",
    "urgent-gradient",
];

const NOT_IN_RINI: &str = "niri has this and rini does not";
const MACOS_DISPLAYS: &str = "macOS owns the display's mode, scale and arrangement; an `output` \
                              here only overrides `layout`'s gaps and struts";
const MACOS_DEVICES: &str = "macOS owns the keyboard and pointing devices; see System Settings";
const ONE_ANIMATION: &str = "rini has one movement animation, timed by `slowdown`; per-animation \
                             settings are not supported";

/// niri's node names that rini refuses, by section path, with the reason.
pub const NIRI: &[(&str, &str, &str)] = &[
    (
        "",
        "prefer-no-csd",
        "macOS draws window decorations; there is nothing to prefer",
    ),
    ("", "screenshot-path", "rini takes no screenshots"),
    (
        "",
        "environment",
        "rini sets no environment; use `spawn-sh-at-startup \"VAR=value command\"`",
    ),
    ("", "clipboard", "rini does not manage the clipboard"),
    (
        "",
        "cursor",
        "macOS owns the cursor's look, and hiding it is not supported yet",
    ),
    ("", "hotkey-overlay", "rini has no hotkey overlay"),
    ("", "blur", "rini draws no blur behind windows"),
    ("", "overview", "rini has no overview"),
    ("", "xwayland-satellite", "there is no Xwayland on macOS"),
    ("", "layer-rule", "macOS has no layer-shell surfaces to rule"),
    ("", "switch-events", "lid and tablet switches are not supported"),
    (
        "",
        "debug",
        "niri's debug options are its renderer's; rini's are actions such as `debug`",
    ),
    (
        "",
        "include",
        "rini reads one file; `include` is not supported yet",
    ),
    ("input", "keyboard", MACOS_DEVICES),
    ("input", "touchpad", MACOS_DEVICES),
    ("input", "mouse", MACOS_DEVICES),
    ("input", "trackpoint", MACOS_DEVICES),
    ("input", "trackball", MACOS_DEVICES),
    ("input", "tablet", MACOS_DEVICES),
    ("input", "touch", MACOS_DEVICES),
    ("input", "disable-power-key-handling", "macOS owns the power key"),
    (
        "input",
        "mod-key-nested",
        "rini never runs nested inside another window manager",
    ),
    ("layout", "always-center-single-column", NOT_IN_RINI),
    (
        "layout",
        "empty-workspace-above-first",
        "rini's workspaces are declared, not dynamic",
    ),
    ("layout", "default-column-display", "rini has no tabbed columns"),
    (
        "layout",
        "background-color",
        "rini draws no background; the desktop shows",
    ),
    ("layout", "preset-window-heights", NOT_IN_RINI),
    ("layout", "shadow", "macOS draws window shadows"),
    ("layout", "tab-indicator", "rini has no tabbed columns"),
    ("layout", "insert-hint", NOT_IN_RINI),
    (
        "layout/default-column-width",
        "fixed",
        "a column's width is a proportion in rini; `fixed` is not supported yet",
    ),
    (
        "layout/min-column-width",
        "fixed",
        "a column's width is a proportion in rini; `fixed` is not supported yet",
    ),
    (
        "layout/max-column-width",
        "fixed",
        "a column's width is a proportion in rini; `fixed` is not supported yet",
    ),
    (
        "layout/preset-column-widths",
        "fixed",
        "a column's width is a proportion in rini; `fixed` is not supported yet",
    ),
    ("animations", "workspace-switch", ONE_ANIMATION),
    ("animations", "window-open", ONE_ANIMATION),
    ("animations", "window-close", ONE_ANIMATION),
    ("animations", "horizontal-view-movement", ONE_ANIMATION),
    ("animations", "window-movement", ONE_ANIMATION),
    ("animations", "window-resize", ONE_ANIMATION),
    ("animations", "config-notification-open-close", ONE_ANIMATION),
    ("animations", "exit-confirmation-open-close", ONE_ANIMATION),
    ("animations", "screenshot-ui-open", ONE_ANIMATION),
    ("animations", "overview-open-close", ONE_ANIMATION),
    ("animations", "recent-windows-close", ONE_ANIMATION),
    ("gestures", "dnd-edge-view-scroll", NOT_IN_RINI),
    ("gestures", "dnd-edge-workspace-switch", NOT_IN_RINI),
    ("gestures", "hot-corners", NOT_IN_RINI),
    (
        "recent-windows",
        "off",
        "not supported yet; leave its binds out instead",
    ),
    ("recent-windows", "debounce-ms", "not supported yet"),
    ("recent-windows", "open-delay-ms", "not supported yet"),
    ("recent-windows", "highlight", "not supported yet"),
    ("recent-windows", "previews", "not supported yet"),
    ("output", "off", MACOS_DISPLAYS),
    ("output", "mode", MACOS_DISPLAYS),
    ("output", "modeline", MACOS_DISPLAYS),
    ("output", "scale", MACOS_DISPLAYS),
    ("output", "transform", MACOS_DISPLAYS),
    ("output", "position", MACOS_DISPLAYS),
    ("output", "variable-refresh-rate", MACOS_DISPLAYS),
    (
        "output",
        "focus-at-startup",
        "not supported yet; `workspace \"name\" { focus-at-startup; }` picks the first workspace",
    ),
    ("output", "background-color", MACOS_DISPLAYS),
    ("output", "backdrop-color", MACOS_DISPLAYS),
    ("output", "max-bpc", MACOS_DISPLAYS),
    ("output", "hot-corners", NOT_IN_RINI),
    (
        "output/layout",
        "focus-ring",
        "the focus ring is the whole file's, in `layout`; an `output`'s `layout` overrides its gaps \
         and struts only",
    ),
    (
        "output/layout",
        "border",
        "the border is the whole file's, in `layout`; an `output`'s `layout` overrides its gaps and \
         struts only",
    ),
    (
        "workspace",
        "open-on-output",
        "rini's workspaces span every display",
    ),
    (
        "workspace",
        "layout",
        "per-workspace layout is not supported yet",
    ),
    (
        "window-rule",
        "open-maximized",
        "not supported yet; `maximize-column` once it opens",
    ),
    ("window-rule", "open-maximized-to-edges", NOT_IN_RINI),
    (
        "window-rule",
        "open-fullscreen",
        "rini leaves native fullscreen to macOS",
    ),
    (
        "window-rule",
        "on-xdg-activate",
        "there is no xdg-activation on macOS",
    ),
    ("window-rule", "min-width", NOT_IN_RINI),
    ("window-rule", "max-width", NOT_IN_RINI),
    ("window-rule", "min-height", NOT_IN_RINI),
    ("window-rule", "max-height", NOT_IN_RINI),
    ("window-rule", "opacity", NOT_IN_RINI),
    ("window-rule", "block-out-from", NOT_IN_RINI),
    ("window-rule", "variable-refresh-rate", NOT_IN_RINI),
    (
        "window-rule",
        "default-column-display",
        "rini has no tabbed columns",
    ),
    ("window-rule", "scroll-factor", NOT_IN_RINI),
    ("window-rule", "pinch-sensitivity", NOT_IN_RINI),
    ("window-rule", "shadow", "macOS draws window shadows"),
    ("window-rule", "tab-indicator", "rini has no tabbed columns"),
    ("window-rule", "clip-to-geometry", "macOS draws the windows"),
    ("window-rule", "tiled-state", NOT_IN_RINI),
    ("window-rule", "baba-is-float", NOT_IN_RINI),
    ("window-rule", "background-effect", NOT_IN_RINI),
    ("window-rule", "popups", NOT_IN_RINI),
    (
        "window-rule/default-column-width",
        "proportion",
        "a window rule sizes in pixels, with `fixed`; `proportion` is not supported yet",
    ),
    (
        "window-rule/default-window-height",
        "proportion",
        "a window rule sizes in pixels, with `fixed`; `proportion` is not supported yet",
    ),
];

const CROSSING_FOCUS: &str = "rini's `focus-window-or-monitor-down` and `-up` cross to the next \
                              display at the column's end; there is no form that stops yet";
const MOVE_WINDOW: &str = "rini's nearest is `move-window-left` or `-right`: a lone window swaps \
                           columns and a stacked one leaves as a new column";
const MOVE_VERTICAL: &str = "rini's nearest is `move-window-down-or-to-monitor-down` or \
                             `move-window-up-or-to-monitor-up`, which cross displays at the column's end";
const FOLD: &str = "rini's nearest is `toggle-fold-left` or `toggle-fold-right`, whose fold-out lands \
                    on the side asked for";
const STACK: &str = "rini's nearest is `toggle-stack`";
const FULLSCREEN: &str =
    "rini leaves native fullscreen to macOS; `maximize-column` fills the tiling area";
const PREVIOUS_DISPLAY: &str =
    "rini has `-next`, which wraps, so with two displays it goes back and forth";
const COLUMN_TO: &str = "rini moves windows, not columns: `move-window-to-workspace` and \
                         `move-window-to-monitor-…`";
const WORKSPACES_DECLARED: &str =
    "rini's workspaces are declared with `workspace \"name\"`, in order";
const SWITCHER: &str =
    "the switcher's `next-window` and `previous-window` go in `recent-windows › binds`";

/// niri's action names that rini does not have, with the reason or rini's nearest.
pub const NIRI_ACTIONS: &[(&str, &str)] = &[
    ("suspend", NOT_IN_RINI),
    ("power-off-monitors", NOT_IN_RINI),
    ("power-on-monitors", NOT_IN_RINI),
    ("toggle-debug-tint", NOT_IN_RINI),
    ("debug-toggle-opaque-regions", NOT_IN_RINI),
    ("debug-toggle-damage", NOT_IN_RINI),
    ("do-screen-transition", NOT_IN_RINI),
    ("screenshot", "rini takes no screenshots"),
    ("screenshot-screen", "rini takes no screenshots"),
    ("screenshot-window", "rini takes no screenshots"),
    (
        "toggle-keyboard-shortcuts-inhibit",
        "macOS has no shortcut-inhibit protocol",
    ),
    ("fullscreen-window", FULLSCREEN),
    ("toggle-windowed-fullscreen", FULLSCREEN),
    ("focus-window-in-column", NOT_IN_RINI),
    ("focus-window-previous", SWITCHER),
    ("focus-column-first", NOT_IN_RINI),
    ("focus-column-last", NOT_IN_RINI),
    ("focus-column-right-or-first", NOT_IN_RINI),
    ("focus-column-left-or-last", NOT_IN_RINI),
    ("focus-column", NOT_IN_RINI),
    (
        "focus-column-or-monitor-left",
        "rini's columns stop at the strip's end; `focus-monitor-left` crosses",
    ),
    (
        "focus-column-or-monitor-right",
        "rini's columns stop at the strip's end; `focus-monitor-right` crosses",
    ),
    ("focus-window-down", CROSSING_FOCUS),
    ("focus-window-up", CROSSING_FOCUS),
    ("focus-window-down-or-column-left", NOT_IN_RINI),
    ("focus-window-down-or-column-right", NOT_IN_RINI),
    ("focus-window-up-or-column-left", NOT_IN_RINI),
    ("focus-window-up-or-column-right", NOT_IN_RINI),
    ("focus-window-or-workspace-down", CROSSING_FOCUS),
    ("focus-window-or-workspace-up", CROSSING_FOCUS),
    ("focus-window-top", NOT_IN_RINI),
    ("focus-window-bottom", NOT_IN_RINI),
    ("focus-window-down-or-top", NOT_IN_RINI),
    ("focus-window-up-or-bottom", NOT_IN_RINI),
    ("move-column-left", MOVE_WINDOW),
    ("move-column-right", MOVE_WINDOW),
    ("move-column-to-first", NOT_IN_RINI),
    ("move-column-to-last", NOT_IN_RINI),
    ("move-column-left-or-to-monitor-left", MOVE_WINDOW),
    ("move-column-right-or-to-monitor-right", MOVE_WINDOW),
    ("move-column-to-index", NOT_IN_RINI),
    ("move-window-down", MOVE_VERTICAL),
    ("move-window-up", MOVE_VERTICAL),
    ("move-window-down-or-to-workspace-down", MOVE_VERTICAL),
    ("move-window-up-or-to-workspace-up", MOVE_VERTICAL),
    ("consume-or-expel-window-left", FOLD),
    ("consume-or-expel-window-right", FOLD),
    ("consume-window-into-column", STACK),
    ("expel-window-from-column", STACK),
    ("swap-window-left", MOVE_WINDOW),
    ("swap-window-right", MOVE_WINDOW),
    ("toggle-column-tabbed-display", "rini has no tabbed columns"),
    ("set-column-display", "rini has no tabbed columns"),
    ("center-window", "rini's nearest is `center-column`"),
    ("center-visible-columns", "rini's nearest is `center-column`"),
    ("move-column-to-workspace-down", COLUMN_TO),
    ("move-column-to-workspace-up", COLUMN_TO),
    ("move-column-to-workspace", COLUMN_TO),
    ("move-workspace-down", WORKSPACES_DECLARED),
    ("move-workspace-up", WORKSPACES_DECLARED),
    ("move-workspace-to-index", WORKSPACES_DECLARED),
    (
        "move-workspace-to-monitor",
        "rini's workspaces span every display",
    ),
    (
        "move-workspace-to-monitor-left",
        "rini's workspaces span every display",
    ),
    (
        "move-workspace-to-monitor-right",
        "rini's workspaces span every display",
    ),
    (
        "move-workspace-to-monitor-down",
        "rini's workspaces span every display",
    ),
    (
        "move-workspace-to-monitor-up",
        "rini's workspaces span every display",
    ),
    (
        "move-workspace-to-monitor-previous",
        "rini's workspaces span every display",
    ),
    (
        "move-workspace-to-monitor-next",
        "rini's workspaces span every display",
    ),
    ("set-workspace-name", WORKSPACES_DECLARED),
    ("unset-workspace-name", WORKSPACES_DECLARED),
    ("focus-monitor-previous", PREVIOUS_DISPLAY),
    ("move-window-to-monitor-previous", PREVIOUS_DISPLAY),
    ("move-column-to-monitor-left", COLUMN_TO),
    ("move-column-to-monitor-right", COLUMN_TO),
    ("move-column-to-monitor-down", COLUMN_TO),
    ("move-column-to-monitor-up", COLUMN_TO),
    ("move-column-to-monitor-previous", COLUMN_TO),
    ("move-column-to-monitor-next", COLUMN_TO),
    ("move-column-to-monitor", COLUMN_TO),
    ("set-window-width", "rini sizes the column: `set-column-width`"),
    ("reset-window-height", NOT_IN_RINI),
    (
        "switch-preset-column-width-back",
        "not supported yet; `switch-preset-column-width` wraps",
    ),
    (
        "switch-preset-window-width",
        "rini sizes the column: `switch-preset-column-width`",
    ),
    (
        "switch-preset-window-width-back",
        "rini sizes the column: `switch-preset-column-width`",
    ),
    ("switch-preset-window-height", NOT_IN_RINI),
    ("switch-preset-window-height-back", NOT_IN_RINI),
    ("maximize-window-to-edges", "rini's nearest is `maximize-column`"),
    ("expand-column-to-available-width", NOT_IN_RINI),
    ("switch-layout", "macOS owns the keyboard layout"),
    ("show-hotkey-overlay", "rini has no hotkey overlay"),
    (
        "move-window-to-floating",
        "rini's nearest is `toggle-window-floating`",
    ),
    (
        "move-window-to-tiling",
        "rini's nearest is `toggle-window-floating`",
    ),
    (
        "focus-floating",
        "rini's nearest is `switch-focus-between-floating-and-tiling`",
    ),
    (
        "focus-tiling",
        "rini's nearest is `switch-focus-between-floating-and-tiling`",
    ),
    ("toggle-window-rule-opacity", NOT_IN_RINI),
    ("set-dynamic-cast-window", NOT_IN_RINI),
    ("set-dynamic-cast-monitor", NOT_IN_RINI),
    ("clear-dynamic-cast-target", NOT_IN_RINI),
    ("toggle-overview", "rini has no overview"),
    ("open-overview", "rini has no overview"),
    ("close-overview", "rini has no overview"),
];

/// Why a niri trigger that is not a key cannot be bound, judged by the key's last segment.
pub fn niri_trigger(spec: &str) -> Option<&'static str> {
    let trigger = spec.rsplit('+').next()?.trim();
    let starts = |prefix: &str| trigger.len() > prefix.len() && trigger.starts_with(prefix);
    if starts("XF86") {
        Some("macOS's media keys do not reach an event tap as keys; `XF86` keys cannot be bound")
    } else if ["Mouse", "Wheel", "Touchpad", "TabletStylus"]
        .iter()
        .any(|prefix| starts(prefix))
    {
        Some("mouse, wheel, touchpad and tablet triggers are not supported yet; a bind takes a key")
    } else {
        None
    }
}

/// Edit distance, for "did you mean".
pub fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1).min(row[j] + 1).min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// The candidate nearest `word`, if it is near enough: within half its length, or 3.
pub fn suggest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let (best, distance) = candidates
        .into_iter()
        .map(|candidate| (candidate, levenshtein(word, candidate)))
        .min_by_key(|(_, distance)| *distance)?;
    (distance <= (best.len() / 2).max(3)).then_some(best)
}

fn children_of(path: &str) -> Option<&'static [&'static str]> {
    SCHEMA
        .iter()
        .find(|(section, _)| *section == path)
        .map(|(_, children)| *children)
}

fn shown(path: &str) -> String { path.replace('/', " › ") }

/// Every node the schema does not have where it is written, as `(node start, problem)`.
///
/// The start is the offset knus labels its own "unexpected node" with, so that report can be
/// replaced by this one.
pub fn unknown_nodes(document: &Document<Span>) -> Vec<(usize, Problem)> {
    let mut out = Vec::new();
    walk(&document.nodes, "", &mut out);
    out
}

fn walk(nodes: &[SpannedNode<Span>], path: &str, out: &mut Vec<(usize, Problem)>) {
    let Some(children) = children_of(path) else { return };
    for node in nodes {
        let name = &**node.node_name;
        if children.contains(&name) {
            let child_path = if path.is_empty() {
                name.to_owned()
            } else {
                format!("{path}/{name}")
            };
            if let Some(grandchildren) = &node.children {
                walk(grandchildren, &child_path, out);
            }
            continue;
        }
        out.push((
            node.span().0,
            unknown_node(path, name, node.node_name.span(), children),
        ));
    }
}

fn unknown_node(path: &str, name: &str, span: &Span, children: &[&str]) -> Problem {
    let message = if path.is_empty() {
        format!("unknown section `{name}`")
    } else {
        format!("unknown node `{name}` in `{}`", shown(path))
    };
    if let Some((_, _, why)) =
        NIRI.iter().find(|(section, niri, _)| *section == path && *niri == name)
    {
        return Problem::at(span, message, "niri's, not rini's").help(*why);
    }
    match suggest(name, children.iter().copied()) {
        Some(near) => Problem::at(span, message, format!("did you mean `{near}`?")),
        None => Problem::at(span, message, "not known here").help(format!(
            "`{}` takes: {}",
            if path.is_empty() { "the file" } else { path },
            children.join(", ")
        )),
    }
}

/// A bind's child that names no rini action.
pub fn unknown_action(name: &str, span: &Span) -> Problem {
    let message = format!("unknown action `{name}`");
    if let Some((_, why)) = NIRI_ACTIONS.iter().find(|(niri, _)| *niri == name) {
        return Problem::at(span, message, "niri's, not rini's").help(*why);
    }
    match suggest(name, Action::VARIANTS.iter().copied()) {
        Some(near) => Problem::at(span, message, format!("did you mean `{near}`?")),
        None => Problem::at(span, message, "not an action")
            .help("the actions are listed in rini.default.kdl"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_counts_edits() {
        assert_eq!(levenshtein("toggle-stak", "toggle-stack"), 1);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn a_near_name_is_suggested_and_a_far_one_is_not() {
        let names = ["focus-column-left", "focus-column-right"];
        assert_eq!(suggest("focus-colum-left", names), Some("focus-column-left"));
        assert_eq!(suggest("quit-everything-now", names), None);
    }

    /// The niri table is about names rini refuses, so none of them may be a rini name.
    #[test]
    fn no_niri_name_is_also_a_rini_name() {
        for (path, name, _) in NIRI {
            let children = children_of(path).unwrap_or_else(|| panic!("`{path}` is not a section"));
            assert!(!children.contains(name), "{path} › {name} is rini's");
        }
        for (name, _) in NIRI_ACTIONS {
            assert!(!Action::VARIANTS.contains(name), "{name} is a rini action");
        }
    }
}
