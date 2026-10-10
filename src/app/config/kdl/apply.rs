//! Laying a decoded part over a `Config`, field by field, and the checks that need more than one
//! node: widths that nest, unique workspace names, names that exist, keys bound once.
//!
//! Singleton sections merge field by field, so a part that leaves a setting out keeps what is
//! there. A part's `workspace` and `window-rule` nodes replace the list, and its binds replace the
//! binds for their keys. The checks across nodes run on the config that results, so a running bind
//! or rule the part does not mention is checked against what the part changes.

use std::collections::BTreeMap;
use std::str::FromStr;

use knus::span::Span;
use rustc_hash::FxHashSet as HashSet;

use super::ConfigPart;
use super::binds::{BindPart, BindsPart};
use super::error::Problem;
use super::layout::{BandPart, GradientPart, GradientRelativeTo, LayoutPart, ProportionPart};
use super::value::{ColourSpaceValue, ColourValue, Flag, Number};
use super::window_rule::{FixedPart, MatchPart, RadiusPart, WindowRulePart};
use crate::app::config::{BASE_ANIMATION_SECONDS, Config, MAX_WORKSPACES};
use crate::decorations::domain::paint::{ColourSpace, Gradient, Paint};
use crate::decorations::domain::rules::{BandRule, DecorationRule};
use crate::decorations::settings::BandSetting;
use crate::input::domain::binding::{Action, Bind};
use crate::input::domain::key::{
    Hotkey, KeyCode, ModKey, Modifiers, normalize_spec, substitute_mod,
};
use crate::windows::domain::rules::{FloatingPosition, RuleMatch, WindowRule};

pub fn apply(part: ConfigPart, config: &mut Config) -> Vec<Problem> {
    let mut problems = Vec::new();
    let settings = &mut config.settings;
    let workspaces = &mut config.virtual_workspaces;

    if let Some(input) = part.input {
        if let Some(mod_key) = input.mod_key {
            settings.mod_key = mod_key;
        }
        if let Some(focus) = input.focus_follows_mouse {
            settings.focus_follows_mouse = focus.on;
            if focus.suspend_while_held.is_some() {
                settings.focus_follows_mouse_disable_hotkey = focus.suspend_while_held;
            }
        }
        if let Some(Flag(on)) = input.warp_mouse_to_focus {
            settings.mouse_follows_focus = on;
        }
        if let Some(Flag(on)) = input.workspace_auto_back_and_forth {
            workspaces.workspace_auto_back_and_forth = on;
        }
        if let Some(warp) = input.warp_mouse_across_stacked_outputs {
            settings.warp_cursor_between_stacked_displays = warp.on;
            if let Some(side) = warp.upper_side {
                settings.stacked_display_upper_is = side;
            }
            if let Some(top) = warp.lower_top_at {
                settings.stacked_display_lower_top_at = top.get();
            }
        }
        if !input.ignore_app_activation.is_empty() {
            settings.auto_focus_denylist = input.ignore_app_activation;
        }
    }

    if let Some(Flag(on)) = part.cursor.and_then(|cursor| cursor.hide_on_focus_change) {
        settings.mouse_hides_on_focus = on;
    }

    if let Some(mut layout) = part.layout {
        if let Some(ring) = layout.focus_ring.take() {
            apply_band(ring, &mut settings.decorations.focus_ring, &mut problems);
        }
        if let Some(border) = layout.border.take() {
            apply_border(border, &mut settings.decorations.border, &mut problems);
        }
        apply_layout(layout, &mut settings.layout, &mut problems);
    }

    for output in part.outputs {
        let Some(layout) = output.layout else { continue };
        let entry = settings.layout.gaps.per_display.entry(output.uuid).or_default();
        if let Some(gaps) = layout.gaps {
            entry.gaps = Some(gaps.get());
        }
        if let Some(struts) = layout.struts {
            entry.struts = Some(struts.edges());
        }
    }

    if let Some(animations) = part.animations {
        if let Some(Flag(off)) = animations.off {
            settings.animate = !off;
        }
        if let Some(slowdown) = animations.slowdown {
            settings.animation_duration = BASE_ANIMATION_SECONDS * slowdown.get();
        }
        if let Some(journal) = animations.journal {
            settings.journal.write = journal.on;
            if let Some(Flag(on)) = journal.measure_handover {
                settings.journal.measure_handover = on;
            }
            settings.journal.measure_handover &= settings.journal.write;
        }
    }

    if let Some(gestures) = part.gestures {
        if let Some(overlap) = gestures.drag_swap_overlap {
            settings.window_snapping.drag_swap_fraction = overlap.get();
        }
    }

    let mut switcher_binds = None;
    if let Some(recent) = part.recent_windows {
        if let Some(icons) = recent.icons {
            settings.switcher.icons = icons
                .into_iter()
                .map(|icon| (icon.bundle_id, icon.path))
                .collect::<BTreeMap<_, _>>();
        }
        switcher_binds = recent.binds;
    }

    if let Some(Flag(off)) = part.bar.and_then(|bar| bar.off) {
        settings.bar.enabled = !off;
    }
    if let Some(Flag(disabled)) = part.config_notification.and_then(|n| n.disable_failed) {
        settings.mark_failed_config = !disabled;
    }

    if let Some(Flag(on)) = part.activate_spaces_by_default {
        settings.default_disable = !on;
    }
    if let Some(Flag(on)) = part.reapply_window_rules_on_title_change {
        workspaces.reapply_app_rules_on_title_change = on;
    }
    if !part.startup.is_empty() {
        settings.run_on_start = part.startup;
    }

    if !part.workspaces.is_empty() {
        apply_workspaces(&part.workspaces, workspaces, &mut problems);
    }

    let names = (!part.undecoded.workspace_names).then(|| workspaces.workspace_names.clone());
    if !part.window_rules.is_empty() {
        let rules: Vec<(WindowRule, DecorationRule)> = part
            .window_rules
            .into_iter()
            .map(|rule| window_rule(rule, names.as_deref(), &mut problems))
            .collect();
        workspaces.app_rules = rules
            .iter()
            .filter(|(opening, decorating)| opening.sets_anything() || !decorating.decorates())
            .map(|(opening, _)| opening.clone())
            .collect();
        settings.decorations.rules =
            rules.into_iter().map(|(_, decorating)| decorating).filter(DecorationRule::decorates).collect();
    } else if !part.undecoded.window_rules
        && let Some(names) = &names
    {
        for name in workspaces.app_rules.iter().filter_map(|rule| rule.open_on_workspace.as_ref()) {
            if !names.contains(name) {
                problems.push(
                    Problem::new(format!(
                        "a running window rule opens on workspace `{name}`, which is not declared"
                    ))
                    .help(declared(names)),
                );
            }
        }
    }

    let checks = BindChecks {
        mod_keys: if part.undecoded.mod_key {
            ModKey::ALL.to_vec()
        } else {
            vec![config.settings.mod_key]
        },
        names,
        running: !part.undecoded.binds,
    };
    apply_binds(part.binds, switcher_binds, &checks, config, &mut problems);
    problems
}

/// A block's `on` and `off`: which one was written, and both is an error.
fn on_off(
    on: Option<Flag>,
    off: Option<Flag>,
    span: &Span,
    problems: &mut Vec<Problem>,
) -> Option<bool> {
    match (on, off) {
        (Some(_), Some(_)) => {
            problems.push(Problem::at(span, "`on` and `off` are both written", "here"));
            None
        }
        (Some(Flag(on)), None) => Some(on),
        (None, Some(Flag(off))) => Some(!off),
        (None, None) => None,
    }
}

/// A band's block laid over its setting, field by field.
fn apply_band(part: BandPart, setting: &mut BandSetting, problems: &mut Vec<Problem>) {
    if let Some(on) = on_off(part.on, part.off, &part.span, problems) {
        setting.on = on;
    }
    if let Some(width) = part.width {
        setting.band.width = width.get();
    }
    if let Some(paint) = paint(part.active_color, part.active_gradient, problems) {
        setting.band.active = paint;
    }
    if let Some(paint) = paint(part.inactive_color, part.inactive_gradient, problems) {
        setting.band.inactive = paint;
    }
    if let Some(paint) = paint(part.urgent_color, part.urgent_gradient, problems) {
        setting.band.urgent = paint;
    }
}

/// A band's paint for one state as one block writes it: its gradient, drawn instead of its colour
/// when both are written, as niri draws it.
fn paint(colour: Option<ColourValue>, gradient: Option<GradientPart>, problems: &mut Vec<Problem>) -> Option<Paint> {
    match gradient {
        Some(gradient) => Some(Paint::Gradient(self::gradient(gradient, problems))),
        None => colour.map(|ColourValue(colour)| Paint::Solid(colour)),
    }
}

/// niri's gradient over its band. One laid over the workspace's view is refused: rini lays each
/// band's gradient over the band alone.
fn gradient(part: GradientPart, problems: &mut Vec<Problem>) -> Gradient {
    if part.relative_to == Some(GradientRelativeTo::WorkspaceView) {
        problems.push(
            Problem::at(&part.span, "`relative-to=\"workspace-view\"` is not supported yet", "here")
                .help("rini lays a gradient over its own band; leave `relative-to` out"),
        );
    }
    Gradient {
        from: part.from.0,
        to: part.to.0,
        angle: f64::from(part.angle),
        space: part.space.map_or_else(ColourSpace::default, |ColourSpaceValue(space)| space),
    }
}

/// niri's `border`, whose block turns the border on unless it writes `off`
/// (`niri:niri-config/src/lib.rs:253-280`, Kai's decision D2 in `specs/config.md`).
fn apply_border(part: BandPart, setting: &mut BandSetting, problems: &mut Vec<Problem>) {
    let switched = part.on.is_some() || part.off.is_some();
    apply_band(part, setting, problems);
    if !switched {
        setting.on = true;
    }
}

fn apply_layout(
    layout: LayoutPart,
    settings: &mut crate::layout::settings::LayoutSettings,
    problems: &mut Vec<Problem>,
) {
    if let Some(gaps) = layout.gaps {
        settings.gaps.gaps = gaps.get();
    }
    if let Some(struts) = layout.struts {
        settings.gaps.struts = struts.edges();
    }
    let scrolling = &mut settings.scrolling;
    let proportion = |part: Option<ProportionPart>,
                      target: &mut f64,
                      problems: &mut Vec<Problem>| {
        let part = part?;
        match part.proportion {
            Some(value) => *target = value.get(),
            None => problems.push(
                Problem::at(&part.span, "expected a size, `{ proportion 0.5; }`", "no size here")
                    .help("rini sizes columns by proportion; a window deciding its own width is not supported yet"),
            ),
        }
        Some(part.span)
    };
    let default = proportion(
        layout.default_column_width,
        &mut scrolling.column_width_ratio,
        problems,
    );
    let min = proportion(
        layout.min_column_width,
        &mut scrolling.min_column_width_ratio,
        problems,
    );
    let max = proportion(
        layout.max_column_width,
        &mut scrolling.max_column_width_ratio,
        problems,
    );
    let (low, mid, high) = (
        scrolling.min_column_width_ratio,
        scrolling.column_width_ratio,
        scrolling.max_column_width_ratio,
    );
    if !(low <= mid && mid <= high) {
        let mut problem = Problem::new(format!(
            "column widths must nest: min-column-width {low} ≤ default-column-width {mid} ≤ max-column-width {high}"
        ));
        for (span, label) in [(min, "min"), (default, "default"), (max, "max")] {
            if let Some(span) = span {
                problem = problem.also(&span, label);
            }
        }
        problems.push(problem);
    }
    if let Some(presets) = layout.preset_column_widths.filter(|presets| !presets.is_empty()) {
        scrolling.preset_column_widths = presets
            .into_iter()
            .map(|super::layout::PresetPart::Proportion(value)| value.get())
            .collect();
    }
    if let Some(center) = layout.center_focused_column {
        scrolling.center_focused_column = center;
    }
    if let Some(point) = layout.window_insertion_point {
        scrolling.window_insertion_point = point;
    }
}

fn apply_workspaces(
    parts: &[super::window_rule::WorkspacePart],
    settings: &mut crate::workspaces::settings::VirtualWorkspaceSettings,
    problems: &mut Vec<Problem>,
) {
    if parts.len() > MAX_WORKSPACES {
        problems.push(Problem::at(
            &parts[MAX_WORKSPACES].span,
            format!("at most {MAX_WORKSPACES} workspaces"),
            "one too many",
        ));
    }
    let mut focused: Option<&Span> = None;
    for (index, workspace) in parts.iter().enumerate() {
        if workspace.name.is_empty() {
            problems.push(Problem::at(&workspace.span, "a workspace needs a name", "empty"));
        }
        if let Some(earlier) = parts[..index]
            .iter()
            .find(|earlier| earlier.name.to_lowercase() == workspace.name.to_lowercase())
        {
            problems.push(
                Problem::at(
                    &workspace.span,
                    format!("workspace `{}` is declared twice", workspace.name),
                    "again here",
                )
                .also(&earlier.span, "first here")
                .help("names are compared ignoring case"),
            );
        }
        if workspace.focus_at_startup.is_some_and(|Flag(on)| on) {
            if let Some(first) = focused {
                problems.push(
                    Problem::at(
                        &workspace.span,
                        "`focus-at-startup` is on two workspaces",
                        "again here",
                    )
                    .also(first, "first here"),
                );
            } else {
                focused = Some(&workspace.span);
                settings.default_workspace = index;
            }
        }
    }
    if focused.is_none() {
        settings.default_workspace = 0;
    }
    settings.workspace_names = parts.iter().map(|workspace| workspace.name.clone()).collect();
}

/// A `window-rule`, as what a window opens with and what it wears.
fn window_rule(
    part: WindowRulePart,
    names: Option<&[String]>,
    problems: &mut Vec<Problem>,
) -> (WindowRule, DecorationRule) {
    let line = |part: MatchPart, problems: &mut Vec<Problem>| {
        let patterns = [&part.app_id, &part.title, &part.app_name];
        if patterns
            .iter()
            .any(|pattern| pattern.as_ref().is_some_and(|p| p.as_str().is_empty()))
        {
            problems.push(
                Problem::at(&part.span, "an empty pattern matches every window", "here")
                    .help("leave the matcher out instead"),
            );
        }
        let names_nothing = patterns.iter().all(|pattern| pattern.is_none())
            && part.ax_role.is_none()
            && part.ax_subrole.is_none()
            && [part.is_modal, part.is_active, part.is_focused, part.is_floating, part.is_active_in_column, part.is_urgent]
                .iter()
                .all(Option::is_none);
        if names_nothing {
            problems.push(
                Problem::at(&part.span, "this line names no matcher", "matches every window")
                    .help("a rule with no `match` line already applies to every window"),
            );
        }
        if part.is_active_in_column.is_some() {
            problems.push(
                Problem::at(&part.span, "`is-active-in-column` is not supported yet", "here")
                    .help("a column keeps no window of its own to focus: focus lands on the row it comes from"),
            );
        }
        RuleMatch {
            app_id: part.app_id,
            title: part.title,
            app_name: part.app_name,
            ax_role: part.ax_role,
            ax_subrole: part.ax_subrole,
            is_modal: part.is_modal,
            is_active: part.is_active,
            is_focused: part.is_focused,
            is_floating: part.is_floating,
            is_urgent: part.is_urgent,
        }
    };
    let matches: Vec<RuleMatch> = part.matches.into_iter().map(|m| line(m, problems)).collect();
    let excludes: Vec<RuleMatch> = part.excludes.into_iter().map(|m| line(m, problems)).collect();

    let open_on_workspace = part.open_on_workspace.map(|workspace| {
        if let Some(names) = names {
            check_workspace_name(&workspace.name, &workspace.span, names, problems);
        }
        workspace.name
    });
    let fixed = |part: Option<FixedPart>, problems: &mut Vec<Problem>| {
        let part = part?;
        if part.fixed.is_none() {
            problems.push(
                Problem::at(&part.span, "expected a size, `{ fixed 640; }`", "no size here")
                    .help("a window deciding its own size is what leaving this out means"),
            );
        }
        part.fixed.map(Number::get)
    };
    let default_column_width = fixed(part.default_column_width, problems);
    let default_window_height = fixed(part.default_window_height, problems);
    if let Some(focused) = &part.open_focused
        && !focused.value
    {
        problems.push(
            Problem::at(
                &focused.span,
                "`open-focused false` is not supported yet",
                "here",
            )
            .help("rini cannot stop a new window taking focus yet; leave it out"),
        );
    }
    if let Some(background) = &part.draw_border_with_background
        && background.value
    {
        problems.push(
            Problem::at(&background.span, "`draw-border-with-background true` is not supported yet", "here")
                .help("rini draws the border and the ring hollow; a fill would go under another application's window"),
        );
    }
    let decorating = DecorationRule {
        matches: matches.clone(),
        excludes: excludes.clone(),
        focus_ring: band_rule(part.focus_ring, problems),
        border: band_rule(part.border, problems),
        corner_radius: part.geometry_corner_radius.and_then(|radius| corner_radius(radius, problems)),
    };
    let decorating = paints_only(decorating, &part.span, problems);
    let opening = WindowRule {
        matches,
        excludes,
        open_on_workspace,
        open_floating: part.open_floating,
        default_floating_position: part.default_floating_position.map(|position| {
            FloatingPosition {
                x: position.x.get(),
                y: position.y.get(),
                relative_to: position.relative_to.unwrap_or_default(),
            }
        }),
        default_column_width,
        default_window_height,
        open_focused: part.open_focused.map(|focused| focused.value),
        manage: part.manage,
        open_on_output: part.open_on_output,
    };
    (opening, decorating)
}

/// A rule's `focus-ring` or `border`: what it writes, `on` and `off` refused together as in `layout`,
/// and no block turning the band on by being written, as niri's rules have it.
fn band_rule(part: Option<BandPart>, problems: &mut Vec<Problem>) -> BandRule {
    let Some(part) = part else { return BandRule::default() };
    BandRule {
        on: on_off(part.on, part.off, &part.span, problems),
        width: part.width.map(Number::get),
        active: paint(part.active_color, part.active_gradient, problems),
        inactive: paint(part.inactive_color, part.inactive_gradient, problems),
        urgent: paint(part.urgent_color, part.urgent_gradient, problems),
    }
}

/// `geometry-corner-radius`: one radius, or four only when they are equal, since every piece rini
/// draws rounds each corner alike.
fn corner_radius(part: RadiusPart, problems: &mut Vec<Problem>) -> Option<f64> {
    let radii: Vec<f64> = part.radii.iter().map(|radius| radius.get()).collect();
    match radii[..] {
        [radius] => Some(radius),
        [first, _, _, _] if radii.iter().all(|radius| *radius == first) => Some(first),
        [_, _, _, _] => {
            problems.push(
                Problem::at(&part.span, "corners of different radii are not supported yet", "here")
                    .help("rini rounds every corner of a window alike; write one radius"),
            );
            None
        }
        _ => {
            problems.push(Problem::at(&part.span, "expected one radius, or four", "here"));
            None
        }
    }
}

/// A rule that applies by whether a window holds focus or is drawn urgent may set paints alone: what
/// else it writes is refused and left out.
fn paints_only(rule: DecorationRule, span: &Span, problems: &mut Vec<Problem>) -> DecorationRule {
    if !rule.follows_focus_or_attention() {
        return rule;
    }
    let mut refuse = |what: &str, why: &str| {
        problems.push(
            Problem::at(span, format!("{what} under `is-active`, `is-focused` or `is-urgent` is not supported"), "in this rule")
                .help(why.to_owned()),
        );
    };
    if rule.border.shapes() {
        refuse(
            "a `border`'s `on`, `off` or `width`",
            "a border that changes with focus or attention lays the strip out again every time either changes; its colours and gradients may follow them",
        );
    }
    if rule.focus_ring.shapes() {
        refuse(
            "a `focus-ring`'s `on`, `off` or `width`",
            "not supported yet: a ring drawn or sized by focus or attention; its colours and gradients may follow them",
        );
    }
    if rule.corner_radius.is_some() {
        refuse("`geometry-corner-radius`", "not supported yet: a corner that changes with focus or attention");
    }
    let colours = |band: BandRule| BandRule { on: None, width: None, ..band };
    DecorationRule {
        focus_ring: colours(rule.focus_ring),
        border: colours(rule.border),
        corner_radius: None,
        ..rule
    }
}

fn check_workspace_name(name: &str, span: &Span, names: &[String], problems: &mut Vec<Problem>) {
    if !names.iter().any(|configured| configured == name) {
        problems.push(
            Problem::at(span, format!("there is no workspace `{name}`"), "not declared")
                .help(declared(names)),
        );
    }
}

fn declared(names: &[String]) -> String { format!("the workspaces are: {}", names.join(", ")) }

/// What the checks across binds read, settled after every other section is applied.
struct BindChecks {
    /// What `Mod` may stand for: `mod-key`, or every modifier while the `input` that sets it could
    /// not be decoded.
    mod_keys: Vec<ModKey>,
    /// The declared workspaces, unless a `workspace` could not be decoded.
    names: Option<Vec<String>>,
    /// Whether the running binds the text leaves are checked: not while a bind block could not be
    /// decoded, since which of them it replaces is not known.
    running: bool,
}

type Chords = HashSet<(Modifiers, KeyCode)>;

impl BindChecks {
    /// Every combination of physical modifiers `key` answers to, one set for each `Mod` it may
    /// stand for: `Alt` is either side or both.
    fn chords(&self, key: &str) -> Result<Vec<Chords>, String> {
        self.mod_keys
            .iter()
            .map(|mod_key| {
                let hotkey = Hotkey::from_str(&substitute_mod(key, *mod_key))
                    .map_err(|error| error.to_string())?;
                Ok(hotkey
                    .modifiers
                    .expand_to_specific()
                    .into_iter()
                    .map(|mods| (mods, hotkey.key_code))
                    .collect())
            })
            .collect()
    }
}

/// Whether two keys are pressed the same way whatever `Mod` stands for.
fn same_key(a: &[Chords], b: &[Chords]) -> bool { a.iter().zip(b).all(|(a, b)| !a.is_disjoint(b)) }

/// A bind this text writes, with the chords it answers to; `bind` is `None` once it is refused.
struct Written<'a> {
    part: &'a BindPart,
    in_switcher: bool,
    bind: Option<Bind>,
    chords: Vec<Chords>,
}

/// The text's binds replace the running ones on the same keys. Both are checked against the
/// `mod-key` and workspaces the whole config ends up with, so a text that changes either cannot
/// leave a running bind on a clashing key or naming a workspace that is gone.
fn apply_binds(
    binds: Option<BindsPart>,
    switcher: Option<BindsPart>,
    checks: &BindChecks,
    config: &mut Config,
    problems: &mut Vec<Problem>,
) {
    let mut written: Vec<Written> = Vec::new();
    for (in_switcher, block) in [(false, &binds), (true, &switcher)] {
        for part in block.iter().flat_map(|block| &block.binds) {
            let key = normalize_spec(part.key.trim());
            let chords = match checks.chords(&key) {
                Ok(chords) => chords,
                Err(error) => {
                    let why = super::suggest::niri_trigger(&key).map_or(error, str::to_owned);
                    problems.push(
                        Problem::at(
                            &part.key_span,
                            format!("`{}` is not a key rini can bind", part.key),
                            "here",
                        )
                        .help(why),
                    );
                    continue;
                }
            };
            if let Some(earlier) = written.iter().find(|earlier| same_key(&earlier.chords, &chords))
            {
                problems.push(
                    Problem::at(&part.key_span, format!("`{}` is bound twice", part.key), "bound again here")
                        .also(&earlier.part.key_span, "first bound here")
                        .help("a key takes one action; keys are compared as pressed, so `Mod` and the modifier it stands for are the same key"),
                );
                continue;
            }
            let bind = part
                .action
                .as_ref()
                .filter(|action| {
                    check_action(action, in_switcher, part, checks.names.as_deref(), problems)
                })
                .map(|action| Bind {
                    key,
                    action: action.clone(),
                    repeat: part.repeat,
                });
            written.push(Written {
                part,
                in_switcher,
                bind,
                chords,
            });
        }
    }

    let running = config.binds.iter().map(|bind| (false, bind));
    let running = running.chain(config.switcher_binds.iter().map(|bind| (true, bind)));
    let mut kept: Vec<(bool, Bind, Vec<Chords>)> = Vec::new();
    for (in_switcher, bind) in running {
        let chords = checks
            .chords(&bind.key)
            .unwrap_or_else(|_| vec![Chords::default(); checks.mod_keys.len()]);
        if written.iter().any(|written| same_key(&written.chords, &chords)) {
            continue;
        }
        if checks.running {
            if let Some((_, earlier, _)) =
                kept.iter().find(|(_, _, earlier)| same_key(earlier, &chords))
            {
                problems.push(
                    Problem::new(format!(
                        "with `mod-key \"{}\"`, the running binds `{}` and `{}` are the same key",
                        config.settings.mod_key.name(),
                        earlier.key,
                        bind.key
                    ))
                    .help("a key takes one action; rebind one of them in the same fragment"),
                );
            }
            if let (Some(names), Some(name)) = (&checks.names, bind.action.workspace_name())
                && !names.iter().any(|declared| declared == name)
            {
                problems.push(
                    Problem::new(format!(
                        "the running bind `{}` names workspace `{name}`, which is not declared",
                        bind.key
                    ))
                    .help(declared(names)),
                );
            }
        }
        kept.push((in_switcher, bind.clone(), chords));
    }

    let (mut binds, mut switcher_binds) = (Vec::new(), Vec::new());
    let kept = kept.into_iter().map(|(in_switcher, bind, _)| (in_switcher, bind));
    let written = written.into_iter().filter_map(|w| Some((w.in_switcher, w.bind?)));
    for (in_switcher, bind) in kept.chain(written) {
        if in_switcher {
            switcher_binds.push(bind);
        } else {
            binds.push(bind);
        }
    }
    config.binds = binds;
    config.switcher_binds = switcher_binds;
}

/// Whether `action` may be bound where it is written; says why not when it may not. A workspace
/// name is checked only when `names` are known.
fn check_action(
    action: &Action,
    in_switcher: bool,
    part: &BindPart,
    names: Option<&[String]>,
    problems: &mut Vec<Problem>,
) -> bool {
    let refuse = |problems: &mut Vec<Problem>, message: String, help: &str| {
        problems.push(Problem::at(&part.span, message, "here").help(help.to_owned()));
        false
    };
    match action {
        Action::NextWindow { .. } | Action::PreviousWindow { .. } if !in_switcher => refuse(
            problems,
            "the switcher's actions go in `recent-windows › binds`".into(),
            "that is where a held modifier keeps the switcher open",
        ),
        _ if in_switcher && !action.is_switcher() => refuse(
            problems,
            "`recent-windows › binds` takes `next-window` and `previous-window`".into(),
            "every other action goes in the top-level `binds`",
        ),
        Action::NextWindow {
            scope: Some(_),
            filter: Some(_),
        }
        | Action::PreviousWindow {
            scope: Some(_),
            filter: Some(_),
        } => refuse(
            problems,
            "a switcher takes `scope=` or `filter=`, not both".into(),
            "filter=\"app-id\" already reaches the application's windows everywhere",
        ),
        Action::Spawn(argv) if argv.is_empty() => refuse(
            problems,
            "`spawn` needs a program".into(),
            "`spawn \"program\" \"arg\"`",
        ),
        _ => match (action.workspace_name(), names) {
            (Some(name), Some(names)) => {
                let before = problems.len();
                check_workspace_name(name, &part.span, names, problems);
                problems.len() == before
            }
            _ => true,
        },
    }
}
