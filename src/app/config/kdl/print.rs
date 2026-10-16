//! A `Config` written back as the file it would be read from, the inverse of `apply`: what
//! `config get` prints. Every setting is written, a default too, each as the file spells it, and
//! the text reads back to the same `Config`. What it cannot keep is in `docs/config.md`.

use std::fmt::Write as _;

use super::value::{ColourValue, Word, colour_space_text};
use crate::app::config::{BASE_ANIMATION_SECONDS, Config, Settings};
use crate::app::startup::StartupCommand;
use crate::decorations::domain::paint::{Colour, Paint};
use crate::decorations::domain::rules::{BandRule, DecorationRule};
use crate::decorations::settings::BandSetting;
use crate::input::domain::binding::{
    Action, Bind, MonitorReference, SizeChange, SwitcherFilter, SwitcherScope, WorkspaceReference,
};
use crate::layout::settings::{Edges, LayoutSettings};
use crate::windows::domain::rules::{RuleMatch, WindowRule};

pub fn print(config: &Config) -> String {
    let mut out = Out::default();
    let settings = &config.settings;
    let workspaces = &config.virtual_workspaces;

    input(&mut out, settings);
    layout(&mut out, settings);
    for (uuid, display) in &settings.layout.gaps.per_display {
        out.block(&format!("output {}", string(&uuid.to_string())), |out| {
            out.block("layout", |out| {
                if let Some(gaps) = display.gaps {
                    out.line(&format!("gaps {gaps}"));
                }
                if let Some(struts) = display.struts {
                    self::struts(out, struts);
                }
            });
        });
    }
    animations(&mut out, settings);
    out.block("gestures", |out| {
        out.line(&format!(
            "drag-swap-overlap {}",
            settings.window_snapping.drag_swap_overlap
        ));
    });
    out.block("recent-windows", |out| {
        out.block("icons", |out| {
            for (bundle_id, path) in &settings.switcher.icons {
                out.line(&format!("{} {}", string(bundle_id), string(path)));
            }
        });
        binds(out, &config.switcher_binds);
    });
    out.block("bar", |out| out.line(&format!("off {}", !settings.bar.enabled)));
    out.block("config-notification", |out| {
        out.line(&format!("disable-failed {}", !settings.mark_failed_config));
    });
    for (index, name) in workspaces.workspace_names.iter().enumerate() {
        let node = format!("workspace {}", string(name));
        if index == workspaces.default_workspace {
            out.line(&format!("{node} {{ focus-at-startup; }}"));
        } else {
            out.line(&node);
        }
    }
    for rule in window_rules(&workspaces.app_rules, &settings.decorations.rules) {
        window_rule(&mut out, rule);
    }
    out.line(&format!(
        "reapply-window-rules-on-title-change {}",
        workspaces.reapply_app_rules_on_title_change
    ));
    out.line(&format!(
        "activate-spaces-by-default {}",
        settings.activate_spaces_by_default
    ));
    for command in &settings.run_on_start {
        match command {
            StartupCommand::Spawn(argv) => out.line(&format!("spawn-at-startup{}", strings(argv))),
            StartupCommand::SpawnSh(line) => {
                out.line(&format!("spawn-sh-at-startup {}", string(line)));
            }
        }
    }
    binds(&mut out, &config.binds);
    out.text
}

fn input(out: &mut Out, settings: &Settings) {
    out.block("input", |out| {
        out.line(&format!("mod-key {}", string(settings.mod_key.name())));
        out.line(&format!("focus-follows-mouse {}", settings.focus_follows_mouse));
        out.line(&format!("warp-mouse-to-focus {}", settings.warp_mouse_to_focus));
        out.line(&format!(
            "warp-mouse-across-stacked-outputs {} upper-side={} lower-top-at={}",
            settings.warp_cursor_between_stacked_displays,
            string(settings.stacked_display_upper_is.word()),
            settings.stacked_display_lower_top_at,
        ));
        for bundle_id in &settings.ignore_app_activation {
            out.line(&format!("ignore-app-activation {}", string(bundle_id)));
        }
    });
}

fn layout(out: &mut Out, settings: &Settings) {
    let LayoutSettings { gaps, scrolling } = &settings.layout;
    out.block("layout", |out| {
        out.line(&format!("gaps {}", gaps.gaps));
        struts(out, gaps.struts);
        let proportion = |name: &str, value: f64| format!("{name} {{ proportion {value}; }}");
        out.line(&proportion("default-column-width", scrolling.column_width_ratio));
        out.line(&proportion("min-column-width", scrolling.min_column_width_ratio));
        out.line(&proportion("max-column-width", scrolling.max_column_width_ratio));
        out.block("preset-column-widths", |out| {
            for width in &scrolling.preset_column_widths {
                out.line(&format!("proportion {width}"));
            }
        });
        out.line(&format!(
            "center-focused-column {}",
            string(scrolling.center_focused_column.word())
        ));
        band(out, "focus-ring", &settings.decorations.focus_ring);
        band(out, "border", &settings.decorations.border);
    });
}

/// niri's `struts`, every side written, since a side left out is 0.
fn struts(out: &mut Out, edges: Edges) {
    out.block("struts", |out| {
        out.line(&format!("left {}", edges.left));
        out.line(&format!("right {}", edges.right));
        out.line(&format!("top {}", edges.top));
        out.line(&format!("bottom {}", edges.bottom));
    });
}

fn band(out: &mut Out, name: &str, setting: &BandSetting) {
    out.block(name, |out| {
        out.line(if setting.on { "on" } else { "off" });
        out.line(&format!("width {}", setting.band.width));
        out.line(&paint("active", setting.band.active));
        out.line(&paint("inactive", setting.band.inactive));
        out.line(&paint("urgent", setting.band.urgent));
    });
}

/// One state's paint: `active-color "#7fc8ff"`, or its gradient.
fn paint(state: &str, fill: Paint) -> String {
    let colour = |colour: Colour| string(&ColourValue(colour).text());
    match fill {
        Paint::Solid(solid) => format!("{state}-color {}", colour(solid)),
        Paint::Gradient(gradient) => format!(
            "{state}-gradient from={} to={} angle={} in={}",
            colour(gradient.from),
            colour(gradient.to),
            gradient.angle,
            string(colour_space_text(gradient.space)),
        ),
    }
}

fn animations(out: &mut Out, settings: &Settings) {
    out.block("animations", |out| {
        out.line(&format!("off {}", !settings.animate));
        let duration = settings.animation_duration;
        out.line(&format!(
            "slowdown {}",
            decimal(duration / BASE_ANIMATION_SECONDS, |slowdown| {
                BASE_ANIMATION_SECONDS * slowdown == duration
            })
        ));
        let journal = &settings.journal;
        if journal.write {
            out.line(&format!(
                "journal {{ measure-handover {}; }}",
                journal.measure_handover
            ));
        } else {
            out.line("journal false");
        }
    });
}

/// A window rule as one `window-rule` writes it: what a window opens with, what it wears, or both
/// when they were one rule.
struct Rule<'a> {
    opening: Option<&'a WindowRule>,
    decorating: Option<&'a DecorationRule>,
}

/// The two lists `apply` splits the rules into, each in its order, written as rules again. An
/// opening rule and a decorating one with the same matchers are written as one.
fn window_rules<'a>(opening: &'a [WindowRule], decorating: &'a [DecorationRule]) -> Vec<Rule<'a>> {
    // An opening rule that sets nothing stays an opening rule only while it dresses nothing.
    let same = |o: &WindowRule, d: &DecorationRule| {
        o.sets_anything() && o.matches == d.matches && o.excludes == d.excludes
    };
    let (mut o, mut d) = (0, 0);
    let mut rules = Vec::new();
    loop {
        let rule = match (opening.get(o), decorating.get(d)) {
            (None, None) => return rules,
            (Some(first), Some(second)) if same(first, second) => Rule {
                opening: Some(first),
                decorating: Some(second),
            },
            (Some(_), Some(second)) if !opening[o..].iter().any(|later| same(later, second)) => {
                Rule {
                    opening: None,
                    decorating: Some(second),
                }
            }
            (Some(first), _) => Rule {
                opening: Some(first),
                decorating: None,
            },
            (None, Some(second)) => Rule {
                opening: None,
                decorating: Some(second),
            },
        };
        o += usize::from(rule.opening.is_some());
        d += usize::from(rule.decorating.is_some());
        rules.push(rule);
    }
}

fn window_rule(out: &mut Out, rule: Rule<'_>) {
    let (matches, excludes) = match (rule.opening, rule.decorating) {
        (Some(opening), _) => (&opening.matches, &opening.excludes),
        (None, Some(decorating)) => (&decorating.matches, &decorating.excludes),
        (None, None) => return,
    };
    out.block("window-rule", |out| {
        for line in matches {
            out.line(&format!("match{}", matchers(line)));
        }
        for line in excludes {
            out.line(&format!("exclude{}", matchers(line)));
        }
        if let Some(opening) = rule.opening {
            opens_with(out, opening);
        }
        if let Some(decorating) = rule.decorating {
            band_rule(out, "focus-ring", &decorating.focus_ring);
            band_rule(out, "border", &decorating.border);
            if let Some(radius) = decorating.corner_radius {
                out.line(&format!("geometry-corner-radius {radius}"));
            }
        }
    });
}

fn opens_with(out: &mut Out, rule: &WindowRule) {
    if let Some(workspace) = &rule.open_on_workspace {
        out.line(&format!("open-on-workspace {}", string(workspace)));
    }
    if let Some(floating) = rule.open_floating {
        out.line(&format!("open-floating {floating}"));
    }
    if let Some(position) = rule.default_floating_position {
        out.line(&format!(
            "default-floating-position x={} y={} relative-to={}",
            position.x,
            position.y,
            string(position.relative_to.word())
        ));
    }
    if let Some(width) = rule.default_column_width {
        out.line(&format!("default-column-width {{ fixed {width}; }}"));
    }
    if let Some(height) = rule.default_window_height {
        out.line(&format!("default-window-height {{ fixed {height}; }}"));
    }
    if let Some(focused) = rule.open_focused {
        out.line(&format!("open-focused {focused}"));
    }
    if let Some(manage) = rule.manage {
        out.line(&format!("manage {manage}"));
    }
    if let Some(role) = rule.open_on_output {
        out.line(&format!("open-on-output {}", string(role.word())));
    }
}

/// A rule's band: only what it sets, so a band it leaves alone is not written.
fn band_rule(out: &mut Out, name: &str, rule: &BandRule) {
    if *rule == BandRule::default() {
        return;
    }
    out.block(name, |out| {
        match rule.on {
            Some(true) => out.line("on"),
            Some(false) => out.line("off"),
            None => {}
        }
        if let Some(width) = rule.width {
            out.line(&format!("width {width}"));
        }
        for (state, set) in [
            ("active", rule.active),
            ("inactive", rule.inactive),
            ("urgent", rule.urgent),
        ] {
            if let Some(set) = set {
                out.line(&paint(state, set));
            }
        }
    });
}

/// A `match` or `exclude` line's properties, each with its leading space.
fn matchers(line: &RuleMatch) -> String {
    let mut text = String::new();
    for (name, pattern) in [
        ("app-id", &line.app_id),
        ("title", &line.title),
        ("app-name", &line.app_name),
    ] {
        if let Some(pattern) = pattern {
            let _ = write!(text, " {name}={}", raw(pattern.as_str()));
        }
    }
    for (name, exact) in [("ax-role", &line.ax_role), ("ax-subrole", &line.ax_subrole)] {
        if let Some(exact) = exact {
            let _ = write!(text, " {name}={}", string(exact));
        }
    }
    for (name, holds) in [
        ("is-modal", line.is_modal),
        ("is-active", line.is_active),
        ("is-focused", line.is_focused),
        ("is-floating", line.is_floating),
        ("is-urgent", line.is_urgent),
    ] {
        if let Some(holds) = holds {
            let _ = write!(text, " {name}={holds}");
        }
    }
    text
}

fn binds(out: &mut Out, binds: &[Bind]) {
    out.block("binds", |out| {
        for bind in binds {
            let repeat = if bind.repeat { " repeat=true" } else { "" };
            out.line(&format!(
                "{}{repeat} {{ {}; }}",
                identifier(&bind.key),
                action(&bind.action)
            ));
        }
    });
}

/// An action as a bind's child node: its name, then what it takes.
fn action(action: &Action) -> String {
    let name: &'static str = action.into();
    let workspace = |reference: &WorkspaceReference| match reference {
        WorkspaceReference::Index(index) => index.to_string(),
        WorkspaceReference::Name(name) => string(name),
    };
    let monitor = |reference: &MonitorReference| match reference {
        MonitorReference::Index(index) => index.to_string(),
        MonitorReference::Uuid(uuid) => string(uuid),
    };
    let skip_empty = |skip: &Option<bool>| skip.map(|skip| format!(" skip-empty={skip}"));
    let switcher = |scope: &Option<SwitcherScope>, filter: &Option<SwitcherFilter>| {
        let scope = scope.map(|scope| match scope {
            SwitcherScope::All => " scope=\"all\"",
            SwitcherScope::Workspace => " scope=\"workspace\"",
        });
        let filter = filter.map(|SwitcherFilter::AppId| " filter=\"app-id\"");
        format!("{}{}", scope.unwrap_or(""), filter.unwrap_or(""))
    };
    let takes = match action {
        Action::SetColumnWidth(change)
        | Action::SetWindowHeight(change)
        | Action::SetColumnWidthOrWindowHeight(change) => {
            format!(" {}", string(&size_change(*change)))
        }
        Action::FocusWorkspace(reference) => format!(" {}", workspace(reference)),
        Action::FocusWorkspaceDown(skip) | Action::FocusWorkspaceUp(skip) => {
            skip_empty(skip).unwrap_or_default()
        }
        Action::MoveWindowToWorkspace(reference, focus) => {
            format!(" {} focus={focus}", workspace(reference))
        }
        Action::MoveWindowToWorkspaceDown(focus) | Action::MoveWindowToWorkspaceUp(focus) => {
            format!(" focus={focus}")
        }
        Action::FocusMonitor(reference)
        | Action::MoveWindowToMonitor(reference)
        | Action::MoveMouseToMonitor(reference) => format!(" {}", monitor(reference)),
        Action::Spawn(argv) => strings(argv),
        Action::SpawnSh(line) => format!(" {}", string(line)),
        Action::NextWindow { scope, filter } | Action::PreviousWindow { scope, filter } => {
            switcher(scope, filter)
        }
        Action::FocusColumnLeft
        | Action::FocusColumnRight
        | Action::FocusWindowOrMonitorDown
        | Action::FocusWindowOrMonitorUp
        | Action::MoveWindowLeft
        | Action::MoveWindowRight
        | Action::MoveWindowDownOrToMonitorDown
        | Action::MoveWindowUpOrToMonitorUp
        | Action::ToggleFoldLeft
        | Action::ToggleFoldRight
        | Action::ExplodeColumn
        | Action::CycleVisibleWindows
        | Action::CycleVisibleWindowsBack
        | Action::ToggleWindowFloating
        | Action::SwitchFocusBetweenFloatingAndTiling
        | Action::MaximizeColumn
        | Action::SwitchPresetColumnWidth
        | Action::CenterColumn
        | Action::SnapStrip
        | Action::FocusWorkspacePrevious
        | Action::FocusMonitorLeft
        | Action::FocusMonitorRight
        | Action::FocusMonitorUp
        | Action::FocusMonitorDown
        | Action::FocusMonitorNext
        | Action::MoveWindowToMonitorLeft
        | Action::MoveWindowToMonitorRight
        | Action::MoveWindowToMonitorUp
        | Action::MoveWindowToMonitorDown
        | Action::MoveWindowToMonitorNext
        | Action::MoveMouseToMonitorLeft
        | Action::MoveMouseToMonitorRight
        | Action::MoveMouseToMonitorUp
        | Action::MoveMouseToMonitorDown
        | Action::MoveMouseToMonitorNext
        | Action::ToggleSpaceActivated
        | Action::CloseWindow
        | Action::LoadConfigFile
        | Action::Quit
        | Action::RedistributeWindows => String::new(),
    };
    format!("{name}{takes}")
}

/// `"+5%"`, in the fewest decimals that read back to the same change.
fn size_change(SizeChange(change): SizeChange) -> String {
    let sign = if change.is_sign_negative() { '-' } else { '+' };
    let percent = decimal(change.abs() * 100.0, |percent| {
        format!("{sign}{percent}%").parse::<SizeChange>() == Ok(SizeChange(change))
    });
    format!("{sign}{percent}%")
}

/// `value` in the fewest decimals that `reads_back` accepts, for a setting held as something
/// computed from what the file writes.
fn decimal(value: f64, reads_back: impl Fn(f64) -> bool) -> String {
    (0..=17)
        .map(|places| format!("{value:.places$}"))
        .find(|text| text.parse().is_ok_and(&reads_back))
        .unwrap_or_else(|| value.to_string())
}

/// Each argument as a string, with its leading space.
fn strings(arguments: &[String]) -> String {
    arguments.iter().map(|argument| format!(" {}", string(argument))).collect()
}

/// A KDL v1 string.
fn string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{c}' => quoted.push_str("\\f"),
            c if c.is_control() => {
                let _ = write!(quoted, "\\u{{{:x}}}", u32::from(c));
            }
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// A KDL v1 raw string, `r"…"` with as many `#` as keep it closed, so a pattern reads as written.
fn raw(text: &str) -> String {
    let hashes = (0..)
        .map(|count| "#".repeat(count))
        .find(|hashes| !text.contains(&format!("\"{hashes}")))
        .unwrap_or_default();
    format!("r{hashes}\"{text}\"{hashes}")
}

/// A node name: bare when KDL v1 reads it as an identifier, quoted otherwise.
fn identifier(name: &str) -> String {
    let allowed = |c: char| c.is_ascii_graphic() && !"\\/(){}<>;[]=,\"!".contains(c) && c != '#';
    let mut chars = name.chars();
    let bare = match (chars.next(), chars.next()) {
        (None, _) => false,
        (Some(first), _) if first.is_ascii_digit() => false,
        (Some('+' | '-'), Some(second)) if second.is_ascii_digit() => false,
        _ => name.chars().all(allowed) && !["true", "false", "null"].contains(&name),
    };
    if bare { name.to_owned() } else { string(name) }
}

/// The text being written, indented four spaces a level.
#[derive(Default)]
struct Out {
    text: String,
    depth: usize,
}

impl Out {
    fn line(&mut self, node: &str) {
        for _ in 0..self.depth {
            self.text.push_str("    ");
        }
        self.text.push_str(node);
        self.text.push('\n');
    }

    fn block(&mut self, head: &str, body: impl FnOnce(&mut Out)) {
        self.line(&format!("{head} {{"));
        self.depth += 1;
        body(self);
        self.depth -= 1;
        self.line("}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_name_is_bare_only_where_kdl_reads_it_as_one() {
        for bare in ["Mod+Shift+H", "Mod+1", "Ctrl+Grave", "F19", "+", "-x"] {
            assert_eq!(identifier(bare), bare);
        }
        for quoted in [
            "1", "+1", "-2x", "true", "null", "a b", "a=b", "a;b", "a\"b", "", "r#x",
        ] {
            assert!(identifier(quoted).starts_with('"'), "{quoted}");
        }
    }

    #[test]
    fn a_pattern_is_a_raw_string_kept_closed() {
        assert_eq!(raw(r"^com\.apple$"), r#"r"^com\.apple$""#);
        assert_eq!(raw(r#"say "hi""#), r##"r#"say "hi""#"##);
        assert_eq!(raw(r##"a"#b"##), r###"r##"a"#b"##"###);
    }

    #[test]
    fn a_string_escapes_what_kdl_v1_escapes() {
        assert_eq!(string("a\"b\\c\nd\te\u{1}"), r#""a\"b\\c\nd\te\u{1}""#);
    }

    /// A value held as something computed from what the file writes prints as the file's number.
    #[test]
    fn a_computed_value_prints_in_the_fewest_decimals_that_read_back() {
        assert_eq!(size_change(SizeChange(0.05)), "+5%");
        assert_eq!(size_change(SizeChange(-0.075)), "-7.5%");
        let duration = BASE_ANIMATION_SECONDS * 0.3;
        assert_eq!(
            decimal(duration / BASE_ANIMATION_SECONDS, |s| BASE_ANIMATION_SECONDS * s
                == duration),
            "0.3"
        );
    }
}
