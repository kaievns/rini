use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use rini_core::ids::DisplayUuid;
use strum::VariantNames;

use super::*;
use crate::app::config::kdl::suggest::{NIRI, NIRI_ACTIONS, SCHEMA};
use crate::decorations::domain::paint::{Colour, ColourSpace, Gradient, Hue, Paint};
use crate::displays::platform::cursor_warp::StackedUpperSide;
use crate::input::domain::binding::{Action, SwitcherFilter, SwitcherScope, WorkspaceReference};
use crate::layout::settings::{CenterFocusedColumn, Edges, Gaps};
use crate::windows::domain::rules::{
    AppRuleDecision, AppRuleEngine, FloatingPosition, RelativeTo, WindowRuleContext,
};
use crate::workspaces::domain::display_pin::DisplayRole;

fn parse(text: &str) -> Config {
    Config::parse("test.kdl", text).unwrap_or_else(|error| panic!("{}", error.render()))
}

/// The rendered report for a text that must be refused, its whitespace folded so a wrapped line
/// still reads as one.
fn refused(text: &str) -> String {
    match Config::parse("test.kdl", text) {
        Ok(_) => panic!("accepted:\n{text}"),
        Err(error) => error.render().split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

/// The settings and workspaces, which are what an empty file and the reference must agree on.
fn settings_and_workspaces(config: &Config) -> (Settings, Vec<String>, usize, bool) {
    let w = &config.virtual_workspaces;
    (
        config.settings.clone(),
        w.workspace_names.clone(),
        w.default_workspace,
        w.reapply_app_rules_on_title_change,
    )
}

// --- the reference -----------------------------------------------------------------------------

#[test]
fn the_embedded_default_parses() {
    if let Err(error) = Config::parse("rini.default.kdl", DEFAULT_KDL) {
        panic!("{}", error.render());
    }
}

/// niri's `diff_empty_to_default`: every value the reference writes is the code default, so it
/// cannot drift from what an empty file means. Binds and window rules are the exception, by design.
#[test]
fn the_reference_means_the_same_settings_and_workspaces_as_an_empty_file() {
    assert_eq!(
        settings_and_workspaces(&Config::reference()),
        settings_and_workspaces(&parse(""))
    );
    assert!(parse("").binds.is_empty(), "a file with no binds has none");
    assert!(parse("").virtual_workspaces.app_rules.is_empty());
    assert!(!Config::reference().binds.is_empty());
}

/// Under `"Shift"` each Mod+Shift bind is its Mod bind, as the reference's comment says.
#[test]
fn the_reference_is_usable_under_every_mod_key_it_offers() {
    for (name, usable) in [
        ("Alt", true),
        ("Ctrl", true),
        ("Super", true),
        ("Shift", false),
    ] {
        let written = format!("mod-key \"{name}\"");
        let text = DEFAULT_KDL.replace("mod-key \"Alt\"", &written);
        assert!(text.contains(&written));
        let file = Config::parse("rini.default.kdl", &text);
        let report = file.as_ref().err().map(ConfigError::render).unwrap_or_default();
        assert_eq!(file.is_ok(), usable, "{name}:\n{report}");
        let fragment =
            Config::reference().with("config set", &format!("input {{\n    {written}\n}}\n"));
        assert_eq!(fragment.is_ok(), usable, "config set {written}");
    }
}

#[test]
fn an_empty_section_means_the_same_as_an_absent_one() {
    let absent = parse("");
    for section in [
        "input",
        "layout",
        "animations",
        "gestures",
        "recent-windows",
        "bar",
        "config-notification",
        "binds",
    ] {
        assert_eq!(parse(&format!("{section} {{\n}}\n")), absent, "`{section} {{}}`");
    }
    assert_eq!(
        parse("recent-windows {\n    icons {\n    }\n    binds {\n    }\n}\n"),
        absent
    );
    assert_eq!(parse("layout {\n    preset-column-widths {\n    }\n}\n"), absent);
}

/// It has one default, whether its section is written or not (`specs/config.md`).
#[test]
fn the_drag_swap_overlap_has_one_default() {
    let overlap = |text: &str| parse(text).settings.window_snapping.drag_swap_overlap;
    assert_eq!(overlap(""), 0.3);
    assert_eq!(overlap("gestures {\n}\n"), 0.3);
    assert_eq!(overlap("gestures {\n    drag-swap-overlap 0.4\n}\n"), 0.4);
}

#[test]
fn the_config_serialises_for_config_get() {
    let json = serde_json::to_value(Config::reference()).expect("serialises");
    assert_eq!(json["settings"]["mod_key"], "Alt");
    assert_eq!(json["binds"][0]["key"], "Mod+Z");
    assert_eq!(json["binds"][0]["action"], "toggle-space-activated");
    assert_eq!(json["virtual_workspaces"]["workspace_names"][0], "Main");
    assert_eq!(json["settings"]["activate_spaces_by_default"], false);
    assert_eq!(json["settings"]["warp_mouse_to_focus"], false);
    assert!(json["settings"]["ignore_app_activation"].is_array());
    assert_eq!(json["settings"]["window_snapping"]["drag_swap_overlap"], 0.3);
}

#[test]
fn the_bar_keeps_its_band_clear_until_it_is_turned_off() {
    let mut settings = Config::reference().settings;
    assert!(settings.bar.enabled);
    assert_eq!(settings.top_band(), crate::bar::domain::layout::HEIGHT);
    settings.bar.enabled = false;
    assert_eq!(settings.top_band(), 0.0);
}

// --- sections ----------------------------------------------------------------------------------

#[test]
fn the_bar_is_turned_off_with_off() {
    assert!(!parse("bar {\n    off\n}\n").settings.bar.enabled);
    assert!(parse("bar {\n    off false\n}\n").settings.bar.enabled);
    assert!(refused("bar {\n    of\n}\n").contains("did you mean `off`?"));
}

#[test]
fn input_maps_onto_focus_and_the_pointer() {
    let config = parse(concat!(
        "input {\n",
        "    mod-key \"Ctrl\"\n",
        "    focus-follows-mouse\n",
        "    warp-mouse-to-focus\n",
        "    warp-mouse-across-stacked-outputs upper-side=\"right\" lower-top-at=0.25\n",
        "    ignore-app-activation \"com.apple.Spotlight\"\n",
        "    ignore-app-activation \"com.apple.dock\"\n",
        "}\n",
    ));
    let s = &config.settings;
    assert_eq!(s.mod_key, ModKey::Ctrl);
    assert!(s.focus_follows_mouse && s.warp_mouse_to_focus);
    assert!(s.warp_cursor_between_stacked_displays);
    assert_eq!(s.stacked_display_upper_is, StackedUpperSide::Right);
    assert_eq!(s.stacked_display_lower_top_at, 0.25);
    assert_eq!(s.ignore_app_activation, [
        "com.apple.Spotlight",
        "com.apple.dock"
    ]);
}

#[test]
fn switching_to_the_shown_workspace_is_not_configurable() {
    let report = refused("input {\n    workspace-auto-back-and-forth\n}\n");
    assert!(
        report.contains("unknown node `workspace-auto-back-and-forth` in `input`"),
        "{report}"
    );
    assert!(report.contains("niri has this and rini does not"), "{report}");
}

#[test]
fn focus_follows_mouse_takes_no_key_that_pauses_it() {
    assert!(parse("input {\n    focus-follows-mouse\n}\n").settings.focus_follows_mouse);
    assert!(
        refused("input {\n    focus-follows-mouse suspend-while-held=\"Fn\"\n}\n")
            .contains("suspend-while-held")
    );
}

#[test]
fn a_cursor_section_is_refused_with_the_reason() {
    for text in ["cursor {\n}\n", "cursor {\n    hide-on-focus-change\n}\n"] {
        let report = refused(text);
        assert!(report.contains("unknown section `cursor`"), "{report}");
        assert!(report.contains("hiding it is not supported yet"), "{report}");
    }
}

#[test]
fn the_two_pointer_flags_are_off_unless_written() {
    let s = parse("").settings;
    assert!(!s.focus_follows_mouse && !s.warp_mouse_to_focus);
}

#[test]
fn a_mod_key_with_no_macos_meaning_is_refused() {
    assert!(
        refused("input {\n    mod-key \"ISO_Level3_Shift\"\n}\n").contains("no meaning on macOS")
    );
}

#[test]
fn layout_maps_onto_the_strip() {
    let config = parse(concat!(
        "layout {\n",
        "    gaps 3\n",
        "    struts { top 20; }\n",
        "    default-column-width { proportion 0.5; }\n",
        "    min-column-width { proportion 0.33333; }\n",
        "    max-column-width { proportion 0.66667; }\n",
        "    preset-column-widths { proportion 0.5; proportion 1.0; }\n",
        "    center-focused-column \"always\"\n",
        "}\n",
    ));
    let layout = &config.settings.layout;
    assert_eq!(layout.gaps.gaps, 3.0);
    assert_eq!(layout.gaps.struts, Edges { top: 20.0, ..Edges::default() });
    let s = &layout.scrolling;
    assert_eq!(
        (
            s.min_column_width_ratio,
            s.column_width_ratio,
            s.max_column_width_ratio
        ),
        (0.33333, 0.5, 0.66667)
    );
    assert_eq!(s.preset_column_widths, [0.5, 1.0]);
    assert_eq!(s.center_focused_column, CenterFocusedColumn::Always);
}

#[test]
fn where_a_new_window_opens_is_not_configurable() {
    let report = refused("layout {\n    window-insertion-point \"end-of-tree\"\n}\n");
    assert!(
        report.contains("unknown node `window-insertion-point` in `layout`"),
        "{report}"
    );
}

#[test]
fn niri_values_rini_lacks_are_refused_by_name() {
    assert!(
        refused("layout {\n    center-focused-column \"on-overflow\"\n}\n")
            .contains("not supported yet")
    );
    assert!(
        refused("layout {\n    default-column-width { fixed 960; }\n}\n")
            .contains("`fixed` is not supported yet")
    );
    assert!(refused("layout {\n    default-column-width {}\n}\n").contains("expected a size"));
}

#[test]
fn column_widths_must_nest_and_the_report_points_at_them() {
    let report = refused(concat!(
        "layout {\n",
        "    min-column-width { proportion 0.8; }\n",
        "    max-column-width { proportion 0.5; }\n",
        "}\n",
    ));
    assert!(report.contains("column widths must nest"), "{report}");
    assert!(report.contains("test.kdl:2:5"), "{report}");
}

#[test]
fn a_number_out_of_range_or_of_the_wrong_kind_says_what_is_expected() {
    assert!(refused("layout {\n    gaps -1\n}\n").contains("expected a number from 0 to 65535"));
    assert!(
        refused("layout {\n    gaps \"wide\"\n}\n").contains("expected a number from 0 to 65535")
    );
    assert!(refused("gestures {\n    drag-swap-overlap 0\n}\n").contains("above 0 to 1"));
}

#[test]
fn gaps_and_struts_make_the_outer_gaps_and_a_display_replaces_each_as_a_whole() {
    let config = parse(concat!(
        "layout {\n    gaps 3\n    struts { left 2; right 2; }\n}\n",
        "output \"D1\" {\n    layout {\n        struts { top 20; }\n    }\n}\n",
        "output \"D2\" {\n    layout {\n        gaps 8\n    }\n}\n",
    ));
    let gaps = &config.settings.layout.gaps;
    assert_eq!(gaps.effective_for_display(None), Gaps {
        outer: Edges {
            top: 3.0,
            left: 5.0,
            bottom: 3.0,
            right: 5.0
        },
        inner: 3.0
    });
    assert_eq!(gaps.effective_for_display(Some(&DisplayUuid::new("D1"))), Gaps {
        outer: Edges {
            top: 23.0,
            left: 3.0,
            bottom: 3.0,
            right: 3.0
        },
        inner: 3.0
    });
    assert_eq!(gaps.effective_for_display(Some(&DisplayUuid::new("D2"))), Gaps {
        outer: Edges {
            top: 8.0,
            left: 10.0,
            bottom: 8.0,
            right: 10.0
        },
        inner: 8.0
    });
    let json = serde_json::to_value(gaps).expect("serialises");
    assert_eq!(
        json["per_display"]["D2"]["gaps"], 8.0,
        "config get keys a display by its UUID"
    );
}

#[test]
fn an_output_takes_only_layout_gaps_and_struts() {
    assert!(refused("output \"D\" {\n    scale 2.0\n}\n").contains("macOS owns"));
    assert!(
        refused(
            "output \"D\" {\n    layout {\n        center-focused-column \"always\"\n    }\n}\n"
        )
        .contains("unknown node `center-focused-column` in `output › layout`")
    );
}

#[test]
fn animations_map_onto_animate_and_the_movement_length() {
    let config = parse("animations {\n    off\n    slowdown 2\n}\n");
    assert!(!config.settings.animate);
    assert_eq!(config.settings.animation_duration, 0.7);
    assert!(
        refused("animations {\n    window-open { duration-ms 150; }\n}\n")
            .contains("one movement animation")
    );
}

#[test]
fn the_journal_is_written_only_when_asked() {
    let written = |text: &str| parse(text).settings.journal.write;
    assert!(!written(""), "absent");
    assert!(!written("animations {\n}\n"), "no node");
    assert!(written("animations {\n    journal\n}\n"));
    assert!(written("animations {\n    journal true\n}\n"));
    assert!(written("animations {\n    journal {\n    }\n}\n"));
    assert!(!written("animations {\n    journal false\n}\n"));
    assert!(refused("animations {\n    jurnal\n}\n").contains("did you mean `journal`?"));
    assert!(
        refused("animations {\n    journal {\n        off\n    }\n}\n")
            .contains("unknown node `off` in `animations › journal`")
    );
}

#[test]
fn the_handover_is_measured_only_when_asked_and_never_off() {
    let journal = |body: &str| {
        parse(&format!("animations {{\n    journal {{\n{body}    }}\n}}\n"))
            .settings
            .journal
    };
    assert!(!parse("").settings.journal.measure_handover, "absent");
    assert!(!journal("").measure_handover);
    let measured = journal("        measure-handover\n");
    assert!(measured.write && measured.measure_handover);
    assert!(!journal("        measure-handover false\n").measure_handover);
    let off = parse("animations {\n    journal false {\n        measure-handover\n    }\n}\n")
        .settings
        .journal;
    assert!(!off.write && !off.measure_handover, "false turns both off");
    assert!(
        refused("animations {\n    journal {\n        measure-handovers\n    }\n}\n")
            .contains("did you mean `measure-handover`?")
    );
}

/// rini taps no trackpad gestures (`specs/input.md`), so the swipes Kai's file configured are names
/// it does not know, refused where they are written.
#[test]
fn the_old_swipe_nodes_are_refused_naming_the_line() {
    let report = refused(concat!(
        "gestures {\n",
        "    swipe-workspace-switch {\n        on\n        fingers 3\n    }\n",
        "    swipe-view-scroll {\n        on\n        fingers 3\n",
        "        switch-workspace-on-overscroll\n    }\n",
        "}\n",
    ));
    assert!(
        report.contains("unknown node `swipe-workspace-switch` in `gestures`"),
        "{report}"
    );
    assert!(report.contains("test.kdl:2:5"), "{report}");
    assert!(
        report.contains("unknown node `swipe-view-scroll` in `gestures`"),
        "{report}"
    );
    assert!(report.contains("test.kdl:6:5"), "{report}");
}

#[test]
fn the_focus_ring_decodes_as_niri_writes_it() {
    let config = parse(
        "layout {\n    focus-ring {\n        width 2.5\n        active-color \"#ff8000\"\n        inactive-color \"rgba(80, 80, 80, 0.5)\"\n    }\n}\n",
    );
    let ring = config.settings.decorations.focus_ring;
    assert!(ring.on, "written without `off`, it stays on");
    assert_eq!(ring.band.width, 2.5);
    assert_eq!(ring.band.active, Paint::Solid(Colour::rgb(0xff8000)));
    assert_eq!(
        ring.band.inactive,
        Paint::Solid(Colour {
            a: 0.5,
            ..Colour::rgb(0x505050)
        })
    );
    let named = parse("layout {\n    focus-ring {\n        active-color \"teal\"\n    }\n}\n");
    assert_eq!(
        named.settings.decorations.focus_ring.band.active,
        Paint::Solid(Colour::rgb(0x008080))
    );
    let off = parse("layout {\n    focus-ring {\n        off\n    }\n}\n");
    assert!(!off.settings.decorations.focus_ring.on);
    assert!(
        refused("layout {\n    focus-ring {\n        active-color \"#zz\"\n    }\n}\n")
            .contains("expected a colour")
    );
}

#[test]
fn the_focus_ring_is_on_by_default_in_niris_colours_at_2pt() {
    let ring = parse("").settings.decorations.focus_ring;
    assert!(ring.on);
    assert_eq!(ring.band.width, 2.0, "Kai's width, not niri's 4");
    assert_eq!(ring.band.active, Paint::Solid(Colour::rgb(0x7fc8ff)));
    assert_eq!(ring.band.inactive, Paint::Solid(Colour::rgb(0x505050)));
    assert_eq!(
        parse("layout {\n    focus-ring {\n    }\n}\n"),
        parse(""),
        "an empty block is an absent one"
    );
}

/// niri takes `on` when both are written; rini refuses the pair, as it does in every other block.
#[test]
fn on_and_off_both_written_are_refused() {
    assert!(
        refused("layout {\n    focus-ring {\n        on\n        off\n    }\n}\n")
            .contains("`on` and `off` are both written")
    );
}

#[test]
fn a_focus_ring_in_an_output_is_refused_with_its_reason() {
    let report = refused(
        "output \"D\" {\n    layout {\n        focus-ring {\n            off\n        }\n    }\n}\n",
    );
    assert!(
        report.contains("the focus ring is the whole file's, in `layout`"),
        "{report}"
    );
}

#[test]
fn the_urgent_colour_and_gradient_decode_as_niri_writes_them() {
    let config = parse(concat!(
        "layout {\n",
        "    focus-ring {\n        urgent-color \"#ff0000\"\n    }\n",
        "    border {\n        urgent-color \"#ff0000\"\n        urgent-gradient from=\"#800\" to=\"#a33\"\n    }\n",
        "}\n",
    ));
    let decorations = &config.settings.decorations;
    assert_eq!(
        decorations.focus_ring.band.urgent,
        Paint::Solid(Colour::rgb(0xff0000))
    );
    let Paint::Gradient(gradient) = decorations.border.band.urgent else {
        panic!("the gradient, ahead of the colour")
    };
    assert_eq!(
        (gradient.from, gradient.to),
        (Colour::rgb(0x880000), Colour::rgb(0xaa3333))
    );
    let ruled =
        parse("window-rule {\n    focus-ring {\n        urgent-color \"#00ff00\"\n    }\n}\n");
    assert_eq!(
        decoration_rule(&ruled).focus_ring.urgent,
        Some(Paint::Solid(Colour::rgb(0x00ff00)))
    );
    let unset = parse("").settings.decorations;
    assert_eq!(
        (unset.focus_ring.band.urgent, unset.border.band.urgent),
        (
            Paint::Solid(Colour::rgb(0x9b0000)),
            Paint::Solid(Colour::rgb(0x9b0000))
        ),
        "niri's"
    );
}

/// The `focus-ring` and `border` blocks of niri's own default config, pasted whole
/// (`niri:resources/default-config.kdl:158-214`), comments and all.
#[test]
fn niris_shipped_border_block_loads() {
    let config = parse(concat!(
        "layout {\n",
        "    focus-ring {\n",
        "        // off\n",
        "        width 4\n",
        "        active-color \"#7fc8ff\"\n",
        "        inactive-color \"#505050\"\n",
        "        // active-gradient from=\"#80c8ff\" to=\"#c7ff7f\" angle=45\n",
        "        // inactive-gradient from=\"#505050\" to=\"#808080\" angle=45 relative-to=\"workspace-view\"\n",
        "    }\n",
        "    border {\n",
        "        off\n",
        "        width 4\n",
        "        active-color \"#ffc87f\"\n",
        "        inactive-color \"#505050\"\n",
        "        urgent-color \"#9b0000\"\n",
        "        // active-gradient from=\"#e5989b\" to=\"#ffb4a2\" angle=45 relative-to=\"workspace-view\" in=\"oklch longer hue\"\n",
        "        // inactive-gradient from=\"#505050\" to=\"#808080\" angle=45 relative-to=\"workspace-view\"\n",
        "    }\n",
        "}\n",
    ));
    let decorations = &config.settings.decorations;
    assert!(decorations.focus_ring.on && !decorations.border.on);
    assert_eq!(
        decorations.border.band,
        crate::decorations::settings::DecorationSettings::default().border.band
    );
}
/// niri's own default config led its users to write `border {}` to turn the border on, so a block
/// with neither `on` nor `off` means on there, and here.
#[test]
fn a_border_with_neither_on_nor_off_turns_it_on() {
    assert!(
        !parse("").settings.decorations.border.on,
        "off unless written, as niri"
    );
    let empty = parse("layout {\n    border {\n    }\n}\n").settings.decorations.border;
    assert!(empty.on);
    let widened = parse(
        "layout {\n    border {\n        width 2\n        active-color \"#ff8000\"\n        inactive-color \"#202020\"\n    }\n}\n",
    );
    let border = widened.settings.decorations.border;
    assert!(border.on);
    assert_eq!(border.band.width, 2.0);
    assert_eq!(
        (border.band.active, border.band.inactive),
        (
            Paint::Solid(Colour::rgb(0xff8000)),
            Paint::Solid(Colour::rgb(0x202020))
        )
    );
    assert_eq!(
        empty.band,
        parse("").settings.decorations.border.band,
        "niri's width and colours"
    );
}

#[test]
fn border_off_stays_off() {
    assert!(
        !parse("layout {\n    border {\n        off\n        width 2\n    }\n}\n")
            .settings
            .decorations
            .border
            .on
    );
    assert!(
        parse("layout {\n    border {\n        on\n    }\n}\n")
            .settings
            .decorations
            .border
            .on
    );
    assert!(
        refused("layout {\n    border {\n        on\n        off\n    }\n}\n")
            .contains("`on` and `off` are both written")
    );
}

#[test]
fn a_border_in_an_output_is_refused_with_its_reason() {
    let report = refused(
        "output \"D\" {\n    layout {\n        border {\n            width 2\n        }\n    }\n}\n",
    );
    assert!(
        report.contains("the border is the whole file's, in `layout`"),
        "{report}"
    );
}

fn ring_paints(text: &str) -> (Paint, Paint) {
    let band = parse(&format!("layout {{\n    focus-ring {{\n{text}    }}\n}}\n"))
        .settings
        .decorations
        .focus_ring
        .band;
    (band.active, band.inactive)
}

#[test]
fn a_gradient_decodes_as_niri_writes_it() {
    let (active, inactive) = ring_paints(
        "        active-gradient from=\"#80c8ff\" to=\"#bbddff\" angle=45 relative-to=\"window\" in=\"oklch longer hue\"\n        inactive-gradient from=\"#505050\" to=\"rgba(128, 128, 128, 0.5)\"\n",
    );
    assert_eq!(
        active,
        Paint::Gradient(Gradient {
            from: Colour::rgb(0x80c8ff),
            to: Colour::rgb(0xbbddff),
            angle: 45.0,
            space: ColourSpace::Oklch(Hue::Longer),
        })
    );
    let Paint::Gradient(inactive) = inactive else {
        panic!("{inactive:?}")
    };
    assert_eq!(
        (inactive.angle, inactive.space),
        (180.0, ColourSpace::Srgb),
        "niri's defaults: top to bottom, in srgb"
    );
    assert_eq!(inactive.to, Colour {
        a: 0.5,
        ..Colour::rgb(0x808080)
    });
    let config = parse(
        "window-rule {\n    border {\n        active-gradient from=\"red\" to=\"blue\" in=\"srgb-linear\"\n    }\n}\n",
    );
    let Some(Paint::Gradient(rule)) = config.settings.decorations.rules[0].border.active else {
        panic!("a rule's gradient")
    };
    assert_eq!(
        (rule.from, rule.to, rule.space),
        (
            Colour::rgb(0xff0000),
            Colour::rgb(0x0000ff),
            ColourSpace::SrgbLinear
        )
    );
}

/// niri draws a part's gradient over its colour, whichever is written first.
#[test]
fn a_gradient_beats_a_colour_in_the_same_part() {
    for text in [
        "        active-color \"#ff0000\"\n        active-gradient from=\"#00ff00\" to=\"#0000ff\"\n",
        "        active-gradient from=\"#00ff00\" to=\"#0000ff\"\n        active-color \"#ff0000\"\n",
    ] {
        let (active, inactive) = ring_paints(text);
        assert!(
            matches!(active, Paint::Gradient(Gradient { from, .. }) if from == Colour::rgb(0x00ff00)),
            "{text}: {active:?}"
        );
        assert_eq!(
            inactive,
            Paint::Solid(Colour::rgb(0x505050)),
            "the other state keeps its colour"
        );
    }
}

#[test]
fn workspace_view_is_refused_as_not_yet_supported() {
    let report = refused(
        "layout {\n    border {\n        active-gradient from=\"red\" to=\"blue\" relative-to=\"workspace-view\"\n    }\n}\n",
    );
    assert!(
        report.contains("`relative-to=\"workspace-view\"` is not supported yet"),
        "{report}"
    );
}

#[test]
fn a_colour_space_is_read_as_niri_reads_it() {
    for (space, problem) in [
        (
            "oklab longer hue",
            "only oklch color space can have hue interpolation",
        ),
        ("oklch longer", "interpolation must end with \"hue\""),
        ("oklch sideways hue", "invalid hue interpolation sideways"),
        (
            "oklch longer hue please",
            "unexpected text after hue interpolation",
        ),
        ("cmyk", "invalid color space cmyk"),
    ] {
        let report = refused(&format!(
            "layout {{\n    focus-ring {{\n        active-gradient from=\"red\" to=\"blue\" in=\"{space}\"\n    }}\n}}\n"
        ));
        assert!(report.contains(problem), "{space}: {report}");
    }
    for (space, expected) in [
        ("oklch", ColourSpace::Oklch(Hue::Shorter)),
        ("oklch decreasing hue", ColourSpace::Oklch(Hue::Decreasing)),
        ("oklab", ColourSpace::Oklab),
    ] {
        let (active, _) = ring_paints(&format!(
            "        active-gradient from=\"red\" to=\"blue\" in=\"{space}\"\n"
        ));
        assert!(
            matches!(active, Paint::Gradient(gradient) if gradient.space == expected),
            "{space}"
        );
    }
}

#[test]
fn recent_windows_icons_are_keyed_by_bundle_id() {
    let config = parse(
        "recent-windows {\n    icons {\n        \"com.mitchellh.ghostty\" \"~/g.png\"\n    }\n}\n",
    );
    assert_eq!(
        config.settings.switcher.icons,
        BTreeMap::from([("com.mitchellh.ghostty".to_owned(), "~/g.png".to_owned())])
    );
}

#[test]
fn startup_commands_keep_their_order_and_their_kind() {
    let config = parse(
        "spawn-at-startup \"open\" \"-a\" \"My App\"\nspawn-sh-at-startup \"echo $HOME | wc -c\"\n",
    );
    assert_eq!(config.settings.run_on_start, [
        StartupCommand::Spawn(vec!["open".into(), "-a".into(), "My App".into()]),
        StartupCommand::SpawnSh("echo $HOME | wc -c".into()),
    ]);
    assert!(refused("spawn-at-startup\n").contains("needs a program"));
}

#[test]
fn the_two_top_level_flags() {
    let config = parse("activate-spaces-by-default\nreapply-window-rules-on-title-change\n");
    assert!(config.settings.activate_spaces_by_default);
    assert!(config.virtual_workspaces.reapply_app_rules_on_title_change);
    assert!(
        !parse("").settings.activate_spaces_by_default,
        "spaces start inactive unless written"
    );
}

#[test]
fn the_failed_config_marker_is_on_unless_disabled() {
    assert!(parse("").settings.mark_failed_config);
    assert!(
        !parse("config-notification {\n    disable-failed\n}\n")
            .settings
            .mark_failed_config
    );
}

// --- workspaces --------------------------------------------------------------------------------

#[test]
fn workspaces_are_declared_in_order_and_one_can_show_at_launch() {
    let config = parse(
        "workspace \"main\"\nworkspace \"comms\" { focus-at-startup; }\nworkspace \"work\"\n",
    );
    assert_eq!(config.virtual_workspaces.workspace_names, [
        "main", "comms", "work"
    ]);
    assert_eq!(config.virtual_workspaces.default_workspace, 1);
    assert_eq!(config.virtual_workspaces.workspace_count(), 3);
}

#[test]
fn workspace_names_are_unique_ignoring_case() {
    let report = refused("workspace \"Main\"\nworkspace \"main\"\n");
    assert!(report.contains("declared twice"), "{report}");
    assert!(
        report.contains("first here") && report.contains("again here"),
        "{report}"
    );
}

#[test]
fn only_one_workspace_shows_at_launch() {
    assert!(
        refused("workspace \"a\" { focus-at-startup; }\nworkspace \"b\" { focus-at-startup; }\n")
            .contains("on two workspaces")
    );
}

#[test]
fn at_most_128_workspaces() {
    let text: String = (0..129).map(|n| format!("workspace \"w{n}\"\n")).collect();
    assert!(refused(&text).contains("at most 128 workspaces"));
}

// --- window rules ------------------------------------------------------------------------------

#[test]
fn a_window_rule_carries_its_matchers_and_properties() {
    let config = parse(concat!(
        "workspace \"main\"\nworkspace \"tools\"\n",
        "window-rule {\n",
        "    match app-id=r\"^com\\.example\\.Tool$\" title=\"Prefs\"\n",
        "    match app-name=\"^Tool$\" ax-subrole=\"AXDialog\" is-modal=false\n",
        "    exclude ax-role=\"AXSheet\"\n",
        "    open-on-workspace \"tools\"\n",
        "    open-floating true\n",
        "    default-floating-position x=40 y=30 relative-to=\"bottom-right\"\n",
        "    default-column-width { fixed 640; }\n",
        "    default-window-height { fixed 480; }\n",
        "    open-focused true\n",
        "    open-on-output \"internal\"\n",
        "}\n",
    ));
    let rule = &config.virtual_workspaces.app_rules[0];
    assert_eq!(rule.matches.len(), 2);
    assert_eq!(
        rule.matches[0].app_id.as_ref().unwrap().as_str(),
        "^com\\.example\\.Tool$"
    );
    assert_eq!(rule.matches[1].ax_subrole.as_deref(), Some("AXDialog"));
    assert_eq!(rule.matches[1].is_modal, Some(false));
    assert_eq!(rule.excludes[0].ax_role.as_deref(), Some("AXSheet"));
    assert_eq!(rule.open_on_workspace.as_deref(), Some("tools"));
    assert_eq!(
        rule.default_floating_position,
        Some(FloatingPosition {
            x: 40.0,
            y: 30.0,
            relative_to: RelativeTo::BottomRight
        })
    );
    assert_eq!(
        (rule.default_column_width, rule.default_window_height),
        (Some(640.0), Some(480.0))
    );
    assert_eq!(rule.open_focused, Some(true));
    assert_eq!(rule.open_on_output, Some(DisplayRole::Internal));
}

/// Every rule that applies is folded in file order, a later one overriding an earlier one.
#[test]
fn rules_from_the_file_fold_in_order() {
    let config = parse(concat!(
        "window-rule {\n    match is-modal=true\n    open-floating true\n}\n",
        "window-rule {\n    match app-id=r\"^com\\.example\\.Editor$\" is-modal=true\n    open-floating false\n}\n",
    ));
    let engine = AppRuleEngine::new(&config.virtual_workspaces.app_rules);
    let modal = |app| WindowRuleContext {
        app_bundle_id: Some(app),
        is_modal: true,
        ..Default::default()
    };
    assert!(matches!(
        engine.evaluate(modal("com.example.Other")),
        AppRuleDecision::Managed { floating: true, .. }
    ));
    assert!(matches!(
        engine.evaluate(modal("com.example.Editor")),
        AppRuleDecision::Managed { floating: false, .. }
    ));
}

#[test]
fn an_invalid_regex_is_an_error_naming_the_line() {
    let report = refused("window-rule {\n    match title=\"(unclosed\"\n}\n");
    assert!(report.contains("test.kdl:2:"), "{report}");
    assert!(report.contains("unclosed group"), "{report}");
}

#[test]
fn a_rule_naming_a_workspace_that_is_not_declared_is_refused() {
    assert!(
        refused("window-rule {\n    match app-id=\"x\"\n    open-on-workspace \"nowhere\"\n}\n")
            .contains("there is no workspace `nowhere`")
    );
}

/// A reference finds its workspace ignoring case, as the duplicate check compares names.
#[test]
fn a_rule_and_a_bind_name_a_workspace_ignoring_case() {
    let config = parse(concat!(
        "workspace \"Main\"\nworkspace \"Tools\"\n",
        "window-rule {\n    match app-id=\"x\"\n    open-on-workspace \"tools\"\n}\n",
        "binds {\n    Mod+1 { focus-workspace \"main\"; }\n}\n",
    ));
    assert_eq!(
        config.virtual_workspaces.app_rules[0].open_on_workspace.as_deref(),
        Some("tools")
    );
    assert!(config.with("config set", "workspace \"MAIN\"\nworkspace \"TOOLS\"\n").is_ok());
}

#[test]
fn rule_shapes_rini_cannot_honour_are_refused() {
    assert!(refused("window-rule {\n    match\n}\n").contains("names no matcher"));
    assert!(refused("window-rule {\n    match title=\"\"\n}\n").contains("empty pattern"));
    assert!(refused("window-rule {\n    open-focused false\n}\n").contains("not supported yet"));
    assert!(
        refused("window-rule {\n    default-column-width { proportion 0.5; }\n}\n")
            .contains("sizes in pixels")
    );
    let report = refused("window-rule {\n    match is-active-in-column=true\n}\n");
    assert!(
        report.contains("`is-active-in-column` is not supported yet")
            && report.contains("a column keeps no window of its own"),
        "{report}"
    );
}

fn decoration_rule(config: &Config) -> &crate::decorations::domain::rules::DecorationRule {
    &config.settings.decorations.rules[0]
}

#[test]
fn a_window_rule_decodes_its_focus_ring_border_and_corner_as_niri_writes_them() {
    let config = parse(concat!(
        "window-rule {\n",
        "    match app-id=\"Ghostty\" is-floating=true\n",
        "    exclude title=\"Prefs\"\n",
        "    focus-ring {\n        off\n    }\n",
        "    border {\n        width 2\n        inactive-color \"#202020\"\n    }\n",
        "    geometry-corner-radius 12\n",
        "    draw-border-with-background false\n",
        "}\n",
    ));
    let rule = decoration_rule(&config);
    assert_eq!(rule.matches[0].is_floating, Some(true));
    assert_eq!(
        rule.excludes[0].title.as_ref().map(|title| title.as_str()),
        Some("Prefs")
    );
    assert_eq!(rule.focus_ring.on, Some(false));
    assert_eq!(
        (rule.border.on, rule.border.width, rule.border.inactive),
        (None, Some(2.0), Some(Paint::Solid(Colour::rgb(0x202020)))),
        "a rule's block turns nothing on"
    );
    assert_eq!(rule.corner_radius, Some(12.0));
    let active = parse(
        "window-rule {\n    match is-active=true\n    exclude is-focused=false\n    border {\n        active-color \"#ff0000\"\n    }\n}\n",
    );
    let rule = decoration_rule(&active);
    assert_eq!(
        (rule.matches[0].is_active, rule.excludes[0].is_focused),
        (Some(true), Some(false))
    );
}

/// A rule's decoration never changes what a window opens with: a rule saying only what it wears
/// leaves an unmatched window to where its application's windows last were.
#[test]
fn a_rule_that_only_decorates_leaves_the_open_time_decision_alone() {
    let config = parse(
        "window-rule {\n    match app-id=\"Ghostty\"\n    border {\n        off\n    }\n}\nwindow-rule {\n    match app-id=\"Zed\"\n}\n",
    );
    assert_eq!(config.settings.decorations.rules.len(), 1);
    let engine = AppRuleEngine::new(&config.virtual_workspaces.app_rules);
    let app = |id| WindowRuleContext {
        app_bundle_id: Some(id),
        ..Default::default()
    };
    assert_eq!(engine.evaluate(app("Ghostty")), AppRuleDecision::NoMatch);
    assert_ne!(
        engine.evaluate(app("Zed")),
        AppRuleDecision::NoMatch,
        "a rule setting nothing applies as before"
    );
}

/// Under a focus matcher a border's width would lay the strip out again on every focus change.
#[test]
fn a_border_width_under_a_focus_matcher_is_refused_with_its_reason() {
    for (line, body) in [
        ("match is-active=true", "width 8"),
        ("exclude is-focused=true", "off"),
        ("match is-focused=false", "on"),
    ] {
        let report = refused(&format!(
            "window-rule {{\n    {line}\n    border {{\n        {body}\n    }}\n}}\n"
        ));
        assert!(
            report.contains("lays the strip out again every time either changes"),
            "{line} {body}: {report}"
        );
    }
    let floating = parse(
        "window-rule {\n    match is-floating=true\n    border {\n        width 8\n    }\n}\n",
    );
    assert_eq!(
        decoration_rule(&floating).border.width,
        Some(8.0),
        "floating is not focus"
    );
}

#[test]
fn a_ring_width_or_a_corner_under_a_focus_matcher_is_refused_as_not_yet_supported() {
    for body in [
        "focus-ring {\n        width 6\n    }",
        "focus-ring {\n        off\n    }",
        "geometry-corner-radius 8",
    ] {
        let report = refused(&format!(
            "window-rule {{\n    match is-active=false\n    {body}\n}}\n"
        ));
        assert!(report.contains("not supported yet"), "{body}: {report}");
    }
}

/// An application asks for attention at rest, so a border sized by it would lay the strip out again
/// each time it asked, as one sized by focus would; its paints may follow it.
#[test]
fn under_is_urgent_only_the_paints_are_taken() {
    let report = refused(
        "window-rule {\n    match is-urgent=true\n    border {\n        width 8\n    }\n}\n",
    );
    assert!(
        report.contains("under `is-active`, `is-focused` or `is-urgent`")
            && report.contains("lays the strip out again"),
        "{report}"
    );
    let config = parse(
        "window-rule {\n    match is-urgent=true\n    border {\n        urgent-color \"#ff0000\"\n    }\n}\n",
    );
    let rule = decoration_rule(&config);
    assert_eq!(rule.matches[0].is_urgent, Some(true));
    assert_eq!(rule.border.urgent, Some(Paint::Solid(Colour::rgb(0xff0000))));
}

#[test]
fn a_colour_under_a_focus_matcher_is_accepted() {
    let config = parse(
        "window-rule {\n    match is-focused=true\n    focus-ring {\n        active-color \"#ff0000\"\n    }\n    border {\n        inactive-color \"#00ff00\"\n    }\n}\n",
    );
    let rule = decoration_rule(&config);
    assert_eq!(
        (rule.focus_ring.active, rule.border.inactive),
        (
            Some(Paint::Solid(Colour::rgb(0xff0000))),
            Some(Paint::Solid(Colour::rgb(0x00ff00)))
        )
    );
}

#[test]
fn four_equal_corner_radii_are_one_radius() {
    let config = parse("window-rule {\n    geometry-corner-radius 10 10 10 10\n}\n");
    assert_eq!(decoration_rule(&config).corner_radius, Some(10.0));
}

/// Every piece rini draws rounds each corner of a window alike.
#[test]
fn unequal_corner_radii_are_refused() {
    assert!(
        refused("window-rule {\n    geometry-corner-radius 12 12 0 0\n}\n")
            .contains("corners of different radii are not supported yet")
    );
    assert!(
        refused("window-rule {\n    geometry-corner-radius 12 12\n}\n")
            .contains("expected one radius, or four")
    );
}

/// rini draws the border and the ring hollow, which is niri's `false`.
#[test]
fn draw_border_with_background_true_is_refused() {
    assert!(
        refused("window-rule {\n    draw-border-with-background true\n}\n")
            .contains("not supported yet")
    );
    parse("window-rule {\n    draw-border-with-background false\n}\n");
}

// --- binds -------------------------------------------------------------------------------------

fn bind_keys(config: &Config) -> Vec<&str> {
    config.binds.iter().map(|bind| bind.key.as_str()).collect()
}

#[test]
fn binds_keep_file_order_and_their_options() {
    let config = parse(concat!(
        "binds {\n",
        "    Mod+L { focus-column-right; }\n",
        "    Mod+H repeat=true { focus-column-left; }\n",
        "    \"Alt + Shift + J\" { move-window-left; }\n",
        "}\n",
    ));
    assert_eq!(bind_keys(&config), ["Mod+L", "Mod+H", "Alt + Shift + J"]);
    assert!(
        !config.binds[0].repeat && config.binds[1].repeat,
        "a held key fires once unless repeat=true"
    );
}

#[test]
fn mod_stands_for_each_mod_key() {
    for (mod_key, expected) in [
        ("Alt", "Alt+J"),
        ("Ctrl", "Ctrl+J"),
        ("Shift", "Shift+J"),
        ("Super", "Super+J"),
    ] {
        let config = parse(&format!(
            "input {{\n    mod-key \"{mod_key}\"\n}}\nbinds {{\n    Mod+J {{ focus-column-left; }}\n}}\n"
        ));
        assert_eq!(config.binds[0].resolved_key(config.settings.mod_key), expected);
    }
}

#[test]
fn a_key_bound_twice_is_an_error_with_both_places() {
    let report = refused(
        "binds {\n    Mod+h { focus-column-left; }\n    Alt+H { focus-column-right; }\n}\n",
    );
    assert!(report.contains("bound twice"), "{report}");
    assert!(
        report.contains("first bound here") && report.contains("bound again here"),
        "{report}"
    );
}

#[test]
fn a_key_is_bound_once_across_both_bind_blocks() {
    let report = refused(concat!(
        "binds {\n    Super+F19 { close-window; }\n}\n",
        "recent-windows {\n    binds {\n        Super+F19 { next-window; }\n    }\n}\n",
    ));
    assert!(report.contains("bound twice"), "{report}");
}

#[test]
fn a_spelling_of_one_side_overlaps_the_generic_modifier() {
    assert!(
        refused("binds {\n    Alt+H { close-window; }\n    LAlt+H { quit; }\n}\n")
            .contains("bound twice")
    );
    parse("binds {\n    LAlt+H { close-window; }\n    RAlt+H { quit; }\n}\n");
}

#[test]
fn niri_triggers_that_are_not_keys_are_refused_with_the_reason() {
    assert!(
        refused("binds {\n    Mod+WheelScrollDown { focus-workspace-down; }\n}\n")
            .contains("triggers are not supported yet")
    );
    assert!(refused("binds {\n    XF86AudioMute { spawn \"true\"; }\n}\n").contains("media keys"));
}

/// niri's default config binds these.
#[test]
fn niri_page_keys_bind() {
    let config = parse(concat!(
        "binds {\n",
        "    Mod+Page_Down { focus-workspace-down; }\n",
        "    Mod+Page_Up { focus-workspace-up; }\n",
        "    Mod+Ctrl+Page_Down { move-window-to-workspace-down; }\n",
        "    Mod+Shift+Page_Up { move-window-to-workspace-up; }\n",
        "}\n",
    ));
    assert_eq!(bind_keys(&config), [
        "Mod+Page_Down",
        "Mod+Page_Up",
        "Mod+Ctrl+Page_Down",
        "Mod+Shift+Page_Up"
    ]);
}

#[test]
fn a_key_with_two_keys_in_it_is_refused() {
    assert!(refused("binds {\n    Ctrl+J+K { close-window; }\n}\n").contains("names two keys"));
}

#[test]
fn a_quoted_key_with_spaces_is_the_same_key() {
    assert!(
        refused("binds {\n    \"Alt + H\" { close-window; }\n    Alt+H { quit; }\n}\n")
            .contains("bound twice")
    );
}

#[test]
fn a_bind_takes_exactly_one_action_and_no_arguments() {
    assert!(refused("binds {\n    Alt+H\n}\n").contains("expected an action"));
    assert!(refused("binds {\n    Alt+H { close-window; quit; }\n}\n").contains("one action"));
    assert!(
        refused("binds {\n    Alt+H \"x\" { close-window; }\n}\n").contains("takes no arguments")
    );
    assert!(
        refused("binds {\n    Alt+H cooldown-ms=150 { close-window; }\n}\n")
            .contains("not supported yet")
    );
    assert!(
        refused("binds {\n    Alt+H allow-when-locked=true { close-window; }\n}\n")
            .contains("lock screen")
    );
}

#[test]
fn the_switcher_actions_belong_in_recent_windows() {
    assert!(
        refused("binds {\n    Super+F19 { next-window; }\n}\n")
            .contains("`recent-windows › binds`")
    );
    assert!(
        refused("recent-windows {\n    binds {\n        Super+F19 { close-window; }\n    }\n}\n")
            .contains("takes `next-window` and `previous-window`")
    );
    assert!(refused("recent-windows {\n    binds {\n        Super+F19 { next-window scope=\"workspace\" filter=\"app-id\"; }\n    }\n}\n")
        .contains("not both"));
    assert!(refused("recent-windows {\n    binds {\n        Super+F19 { next-window scope=\"output\"; }\n    }\n}\n")
        .contains("span every display"));
}

#[test]
fn the_switchers_three_scopes_decode() {
    let config = parse(concat!(
        "recent-windows {\n    binds {\n",
        "        Super+F19 { next-window; }\n",
        "        Super+F18 { next-window scope=\"workspace\"; }\n",
        "        Super+F17 { previous-window filter=\"app-id\"; }\n",
        "    }\n}\n",
    ));
    let actions: Vec<_> = config.switcher_binds.iter().map(|bind| bind.action.clone()).collect();
    assert_eq!(actions, [
        Action::NextWindow { scope: None, filter: None },
        Action::NextWindow {
            scope: Some(SwitcherScope::Workspace),
            filter: None
        },
        Action::PreviousWindow {
            scope: None,
            filter: Some(SwitcherFilter::AppId)
        },
    ]);
}

#[test]
fn workspace_indices_start_at_one() {
    let config = parse("binds {\n    Mod+1 { focus-workspace 1; }\n}\n");
    assert_eq!(
        config.binds[0].action,
        Action::FocusWorkspace(WorkspaceReference::Index(1))
    );
    let report = refused("binds {\n    Mod+0 { focus-workspace 0; }\n}\n");
    assert!(report.contains("start at 1"), "{report}");
}

/// specs/workspaces.md: niri's default is to follow, and the shipped binds send the window away.
#[test]
fn a_move_to_a_workspace_follows_unless_focus_false_and_the_default_binds_do_not_follow() {
    let config = parse(concat!(
        "binds {\n",
        "    Mod+1 { move-window-to-workspace 1; }\n",
        "    Mod+2 { move-window-to-workspace 2 focus=false; }\n",
        "    Mod+3 { move-window-to-workspace-down; }\n",
        "    Mod+4 { move-window-to-workspace-up; }\n",
        "}\n",
    ));
    let actions: Vec<Action> = config.binds.into_iter().map(|bind| bind.action).collect();
    assert_eq!(actions, [
        Action::MoveWindowToWorkspace(WorkspaceReference::Index(1), true),
        Action::MoveWindowToWorkspace(WorkspaceReference::Index(2), false),
        Action::MoveWindowToWorkspaceDown(true),
        Action::MoveWindowToWorkspaceUp(true),
    ]);

    let shipped: Vec<(String, bool)> = Config::reference()
        .binds
        .into_iter()
        .filter_map(|bind| match bind.action {
            Action::MoveWindowToWorkspace(_, follow)
            | Action::MoveWindowToWorkspaceDown(follow)
            | Action::MoveWindowToWorkspaceUp(follow) => Some((bind.key, follow)),
            _ => None,
        })
        .collect();
    assert_eq!(shipped.len(), 4, "{shipped:?}");
    assert!(shipped.iter().all(|(_, follow)| !follow), "{shipped:?}");
}

#[test]
fn a_bind_naming_a_workspace_that_is_not_declared_is_refused() {
    let report =
        refused("workspace \"main\"\nbinds {\n    Mod+1 { focus-workspace \"mian\"; }\n}\n");
    assert!(report.contains("there is no workspace `mian`"), "{report}");
    parse("workspace \"next\"\nbinds {\n    Mod+1 { move-window-to-workspace \"next\"; }\n}\n");
}

#[test]
fn no_action_adds_a_workspace_the_config_does_not_name() {
    let report = refused("binds {\n    Mod+N { create-workspace; }\n}\n");
    assert!(report.contains("unknown action `create-workspace`"), "{report}");
}

#[test]
fn a_misspelt_action_gets_a_suggestion_on_its_span() {
    let report = refused("binds {\n    Mod+J { focus-colum-left; }\n}\n");
    assert!(report.contains("unknown action `focus-colum-left`"), "{report}");
    assert!(report.contains("did you mean `focus-column-left`?"), "{report}");
    assert!(report.contains("test.kdl:2:13"), "{report}");
}

#[test]
fn scroll_strip_is_not_an_action() {
    let report = refused("binds {\n    Mod+S { scroll-strip 0.5; }\n}\n");
    assert!(report.contains("unknown action `scroll-strip`"), "{report}");
}

#[test]
fn a_niri_action_rini_lacks_is_told_why() {
    let report = refused("binds {\n    Mod+J { focus-window-down; }\n}\n");
    assert!(report.contains("cross to the next display"), "{report}");
}

#[test]
fn a_niri_debug_section_points_at_nothing_rini_refuses() {
    let report = refused("debug {\n}\n");
    let (_, help) = report.split_once("help:").expect("a help line");
    assert!(help.contains("rini has no debug options or actions"), "{report}");
    assert!(!help.contains("`debug`"), "{report}");
}

/// Kai deleted the debug actions on 2026-10-15 ("The pictures held" in `specs/animation.md`), so a
/// bind naming one is an unknown action.
#[test]
fn the_debug_actions_are_gone() {
    for name in [
        "debug",
        "serialize",
        "show-timing",
        "debug-warm-snapshots",
        "debug-overlay-slide",
    ] {
        let report = refused(&format!("binds {{\n    Mod+J {{ {name}; }}\n}}\n"));
        assert!(
            report.contains(&format!("unknown action `{name}`")),
            "{name}: {report}"
        );
    }
}

/// Kai deleted the scoped manual layouts on 2026-10-15 (`specs/persistence.md`).
#[test]
fn no_action_saves_or_restores_a_layout_file() {
    for bind in [
        "save-layout \"/tmp/l.ron\"",
        "restore-layout \"/tmp/l.ron\" scope=\"space\"",
    ] {
        let report = refused(&format!("binds {{\n    Mod+J {{ {bind}; }}\n}}\n"));
        let name = bind.split_once(' ').unwrap().0;
        assert!(
            report.contains(&format!("unknown action `{name}`")),
            "{name}: {report}"
        );
    }
}

/// Kai deleted the native Space switch on 2026-10-15 ("Native Spaces" in `specs/displays.md`).
#[test]
fn no_action_switches_the_native_space() {
    for name in ["switch-space-left", "switch-space-right"] {
        let report = refused(&format!("binds {{\n    Mod+J {{ {name}; }}\n}}\n"));
        assert!(
            report.contains(&format!("unknown action `{name}`")),
            "{name}: {report}"
        );
    }
}

/// Kai kept only the explode half of `toggle-stack` on 2026-10-15 ("Folding" in `specs/strip.md`),
/// so the old name is told where both halves went.
#[test]
fn toggle_stack_is_refused_for_explode_column_and_the_left_fold() {
    let report = refused("binds {\n    Mod+Shift+Comma { toggle-stack; }\n}\n");
    assert!(report.contains("unknown action `toggle-stack`"), "{report}");
    assert!(report.contains("no longer rini's"), "{report}");
    let (_, help) = report.split_once("help:").expect("a help line");
    assert!(help.contains("`explode-column`"), "{report}");
    assert!(help.contains("`toggle-fold-left`"), "{report}");
}

/// Every action decodes from its name, with the arguments it needs.
#[test]
fn every_action_name_decodes() {
    let arguments = |name: &str| match name {
        "set-column-width" | "set-window-height" | "set-column-width-or-window-height" => {
            " \"+5%\""
        }
        "focus-workspace"
        | "move-window-to-workspace"
        | "focus-monitor"
        | "move-window-to-monitor"
        | "move-mouse-to-monitor" => " 1",
        "spawn" | "spawn-sh" => " \"true\"",
        _ => "",
    };
    for (index, name) in Action::VARIANTS.iter().enumerate() {
        let key = format!(
            "{}F{}",
            ["", "Ctrl+", "Alt+", "Shift+"][index / 20],
            index % 20 + 1
        );
        let bind = format!("        {key} {{ {name}{}; }}\n", arguments(name));
        let text = if matches!(*name, "next-window" | "previous-window") {
            format!("recent-windows {{\n    binds {{\n{bind}    }}\n}}\n")
        } else {
            format!("binds {{\n{bind}}}\n")
        };
        let config = parse(&text);
        let action =
            config.binds.iter().chain(&config.switcher_binds).next().unwrap().action.clone();
        let json = serde_json::to_value(&action).unwrap();
        let decoded = json
            .as_str()
            .map(str::to_owned)
            .or_else(|| json.as_object().and_then(|o| o.keys().next().cloned()));
        assert_eq!(decoded.as_deref(), Some(*name), "{text}");
    }
}

// --- errors ------------------------------------------------------------------------------------

#[test]
fn an_unknown_node_is_an_error_with_its_line_and_a_suggestion() {
    let report = refused("layout {\n    gasp 3\n}\n");
    assert!(report.contains("unknown node `gasp` in `layout`"), "{report}");
    assert!(report.contains("did you mean `gaps`?"), "{report}");
    assert!(report.contains("test.kdl:2:5"), "{report}");
    assert_eq!(
        report.matches("gasp").count(),
        2,
        "one report, not knus's as well:\n{report}"
    );
}

#[test]
fn an_unknown_section_names_the_sections_when_nothing_is_near() {
    let report = refused("completely-different {\n}\n");
    assert!(
        report.contains("unknown section `completely-different`"),
        "{report}"
    );
    assert!(report.contains("takes: input, layout"), "{report}");
}

#[test]
fn a_niri_section_is_told_why_rather_than_offered_a_lookalike() {
    let report = refused("prefer-no-csd\n");
    assert!(report.contains("macOS draws window decorations"), "{report}");
}

#[test]
fn every_problem_in_the_file_is_reported_at_once() {
    let report = refused(concat!(
        "layout {\n    gasp 3\n}\n",
        "binds {\n    Mod+J { focus-colum-left; }\n}\n",
        "bar {\n    of\n}\n",
    ));
    for expected in ["`gasp`", "`focus-colum-left`", "`of`"] {
        assert!(report.contains(expected), "{expected} missing:\n{report}");
    }
}

#[test]
fn what_does_not_decode_and_the_checks_across_nodes_are_one_report() {
    let report = refused(concat!(
        "layout {\n    gasp 3\n    gaps -1\n}\n",
        "layout {\n}\n",
        "binds {\n",
        "    Mod+J { focus-column-left; }\n",
        "    Mod+J { focus-column-right; }\n",
        "    Ctrl+J+K { close-window; }\n",
        "    Mod+Q { focus-colum-left; }\n",
        "}\n",
    ));
    for expected in [
        "`gasp`",
        "from 0 to 65535",
        "`layout` is written twice",
        "`Mod+J` is bound twice",
        "`Ctrl+J+K` is not a key rini can bind",
        "`focus-colum-left`",
    ] {
        assert!(report.contains(expected), "{expected} missing:\n{report}");
    }
}

#[test]
fn a_bind_whose_action_does_not_decode_still_holds_its_key() {
    let report =
        refused("binds {\n    Mod+J { focus-colum-left; }\n    Mod+J { focus-column-right; }\n}\n");
    assert!(report.contains("`focus-colum-left`"), "{report}");
    assert!(report.contains("`Mod+J` is bound twice"), "{report}");
}

/// A line from niri's default config.
#[test]
fn a_wheel_trigger_with_a_cooldown_is_told_both_reasons() {
    let report =
        refused("binds {\n    Mod+WheelScrollDown cooldown-ms=150 { focus-workspace-down; }\n}\n");
    assert!(report.contains("`cooldown-ms` is not supported yet"), "{report}");
    assert!(
        report.contains("wheel, touchpad and tablet triggers"),
        "{report}"
    );
}

/// Run on what was there instead, these checks would report problems the file does not have.
#[test]
fn a_check_that_needs_a_node_that_did_not_decode_is_not_run() {
    let report = refused(concat!(
        "input {\n    mod-key \"Ctrl\"\n    warp-mouse-to-focus 3\n}\n",
        "workspace \"a\" {\n    focus-at-startup 3\n}\n",
        "window-rule {\n    match app-id=\"x\"\n    open-on-workspace \"a\"\n}\n",
        "binds {\n",
        "    Mod+J { focus-workspace \"a\"; }\n",
        "    Alt+J { close-window; }\n",
        "    Mod+K { close-window; }\n",
        "    Mod+K { quit; }\n",
        "}\n",
    ));
    assert!(!report.contains("`Alt+J` is bound twice"), "{report}");
    assert!(!report.contains("there is no workspace"), "{report}");
    assert!(
        report.contains("`Mod+K` is bound twice"),
        "one key whatever mod-key says: {report}"
    );
}

#[test]
fn a_section_written_twice_is_an_error_with_both_places() {
    let report = refused("layout {\n    gaps 1\n}\nlayout {\n    gaps 2\n}\n");
    assert!(report.contains("`layout` is written twice"), "{report}");
    assert!(
        report.contains("first written here") && report.contains("written again here"),
        "{report}"
    );
}

#[test]
fn kdl_v2_syntax_is_a_syntax_error() {
    let report = refused("animations {\n    off #true\n}\n");
    assert!(report.contains("test.kdl:2:"), "{report}");
}

/// `SCHEMA` lists only names the decoder takes, and `NIRI` only names it refuses: neither table can
/// promise something the decoder does not do.
#[test]
fn the_suggestion_tables_are_true() {
    use knus::span::Span;
    // The decoder's own verdict, before the walk replaces its unknown-node reports.
    let decoder_refuses = |path: &str, name: &str| -> bool {
        let mut text = format!("{name}\n");
        for section in
            path.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().into_iter().rev()
        {
            let opener = match section {
                "output" => "output \"D\"".to_owned(),
                "workspace" => "workspace \"w\"".to_owned(),
                other => other.to_owned(),
            };
            text = format!("{opener} {{\n{text}}}\n");
        }
        let document = knus::parse_ast::<Span>("t.kdl", &text).expect("syntax");
        let node = &document.nodes[0];
        let section = if path.is_empty() {
            &**node.node_name
        } else {
            path.split('/').next().unwrap()
        };
        // Refused as a node (a struct's unknown child) or as a variant (an enum's unknown name),
        // either way reported where the node starts.
        let at = text.find(&format!("{name}\n")).expect("the node is in the text");
        let unexpected = |errors: Vec<knus::errors::DecodeError<Span>>| {
            errors.iter().any(|error| match error {
                knus::errors::DecodeError::Unexpected { span, kind: "node", .. }
                | knus::errors::DecodeError::Conversion { span, .. } => span.0 == at,
                _ => false,
            })
        };
        use super::kdl::{bar, gestures, input, layout, window_rule};
        let result = match section {
            "input" => knus::decode::node::<input::InputPart, Span>(node).err(),
            "layout" => knus::decode::node::<layout::LayoutPart, Span>(node).err(),
            "animations" => knus::decode::node::<gestures::AnimationsPart, Span>(node).err(),
            "gestures" => knus::decode::node::<gestures::GesturesPart, Span>(node).err(),
            "recent-windows" => knus::decode::node::<bar::RecentWindowsPart, Span>(node).err(),
            "bar" => knus::decode::node::<bar::BarPart, Span>(node).err(),
            "output" => knus::decode::node::<layout::OutputPart, Span>(node).err(),
            "workspace" => knus::decode::node::<window_rule::WorkspacePart, Span>(node).err(),
            "window-rule" => knus::decode::node::<window_rule::WindowRulePart, Span>(node).err(),
            _ => {
                return Config::parse("t.kdl", &text)
                    .is_err_and(|error| error.render().contains("unknown section"));
            }
        };
        result.is_some_and(unexpected)
    };
    for (path, children) in SCHEMA {
        for child in *children {
            assert!(
                !decoder_refuses(path, child),
                "{path} › {child} is in SCHEMA but refused"
            );
        }
    }
    for (path, name, _) in NIRI {
        assert!(
            decoder_refuses(path, name),
            "{path} › {name} is in NIRI but accepted"
        );
        assert!(
            refused_somewhere(path, name),
            "{path} › {name} gets no pointed message"
        );
    }
    for (name, why) in NIRI_ACTIONS {
        let report = refused(&format!("binds {{\n    Alt+F1 {{ {name}; }}\n}}\n"));
        let why = why.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(report.contains(&why), "{name}:\n{report}");
    }
}

fn refused_somewhere(path: &str, name: &str) -> bool {
    let mut text = format!("{name}\n");
    for section in path.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().into_iter().rev() {
        let opener = match section {
            "output" => "output \"D\"".to_owned(),
            "workspace" => "workspace \"w\"".to_owned(),
            other => other.to_owned(),
        };
        text = format!("{opener} {{\n{text}}}\n");
    }
    refused(&text).contains("niri's, not rini's")
}

// --- fragments ---------------------------------------------------------------------------------

#[test]
fn a_fragment_changes_what_it_names_and_keeps_the_rest() {
    let running = Config::reference();
    let changed = running.with("config set", "layout {\n    gaps 8\n}\n").expect("applies");
    assert_eq!(changed.settings.layout.gaps.gaps, 8.0);
    assert_eq!(
        changed.settings.layout.scrolling,
        running.settings.layout.scrolling
    );
    assert_eq!(changed.binds, running.binds);
}

#[test]
fn a_fragments_binds_replace_the_binds_for_their_keys() {
    let running = Config::reference();
    let changed = running
        .with("config set", "binds {\n    Mod+H { close-window; }\n}\n")
        .expect("applies");
    assert_eq!(changed.binds.len(), running.binds.len());
    let h = changed.binds.iter().find(|bind| bind.key == "Mod+H").unwrap();
    assert_eq!(h.action, Action::CloseWindow);
}

fn refused_fragment(running: &Config, fragment: &str) -> String {
    match running.with("config set", fragment) {
        Ok(_) => panic!("accepted:\n{fragment}"),
        Err(error) => error.render().split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

#[test]
fn a_fragment_that_makes_two_running_binds_one_key_is_refused() {
    let running =
        parse("binds {\n    Mod+J { focus-column-left; }\n    Ctrl+J { close-window; }\n}\n");
    let report = refused_fragment(&running, "input {\n    mod-key \"Ctrl\"\n}\n");
    assert!(
        report.contains("the running binds `Mod+J` and `Ctrl+J` are the same key"),
        "{report}"
    );
    let rebound = running
        .with(
            "config set",
            "input {\n    mod-key \"Ctrl\"\n}\nbinds {\n    Ctrl+J { close-window; }\n}\n",
        )
        .expect("rebinding the key in the same fragment settles it");
    assert_eq!(bind_keys(&rebound), ["Ctrl+J"]);
}

#[test]
fn a_fragment_that_drops_a_workspace_a_running_bind_or_rule_names_is_refused() {
    let running = parse(concat!(
        "workspace \"a\"\nworkspace \"b\"\n",
        "window-rule {\n    match app-id=\"x\"\n    open-on-workspace \"b\"\n}\n",
        "binds {\n    Mod+B { focus-workspace \"b\"; }\n}\n",
    ));
    let report = refused_fragment(&running, "workspace \"a\"\nworkspace \"c\"\n");
    assert!(
        report.contains("the running bind `Mod+B` names workspace `b`"),
        "{report}"
    );
    assert!(
        report.contains("a running window rule opens on workspace `b`"),
        "{report}"
    );
    assert!(
        running
            .with(
                "config set",
                "workspace \"a\"\nworkspace \"b\"\nworkspace \"c\"\n"
            )
            .is_ok()
    );
}

/// The list a fragment's `window-rule` or bind block would have replaced is not known, so the
/// running rules or binds are not checked; the ones it leaves alone still are.
#[test]
fn a_fragment_whose_rules_or_binds_did_not_decode_does_not_check_the_running_ones() {
    let running = parse(concat!(
        "workspace \"a\"\nworkspace \"b\"\n",
        "window-rule {\n    match app-id=\"x\"\n    open-on-workspace \"b\"\n}\n",
        "binds {\n    Mod+B { focus-workspace \"b\"; }\n    Ctrl+J { close-window; }\n}\n",
        "recent-windows {\n    binds {\n        Mod+J { next-window; }\n    }\n}\n",
    ));

    let report = refused_fragment(
        &running,
        "workspace \"a\"\nwindow-rule {\n    match app-id=\"y\"\n    bogus 1\n}\n",
    );
    assert!(report.contains("`bogus`"), "{report}");
    assert!(!report.contains("a running window rule"), "{report}");
    assert!(
        report.contains("the running bind `Mod+B` names workspace `b`"),
        "{report}"
    );

    let report = refused_fragment(
        &running,
        concat!(
            "workspace \"a\"\nbinds {\n}\n",
            "binds {\n    Mod+B { focus-workspace \"a\"; }\n}\n",
        ),
    );
    assert!(report.contains("`binds` is written twice"), "{report}");
    assert!(!report.contains("the running bind"), "{report}");
    assert!(
        report.contains("a running window rule opens on workspace `b`"),
        "{report}"
    );

    let report = refused_fragment(
        &running,
        concat!(
            "input {\n    mod-key \"Ctrl\"\n}\n",
            "recent-windows {\n    bogus 1\n    binds {\n        Alt+J { next-window; }\n    }\n}\n",
        ),
    );
    assert!(report.contains("`bogus`"), "{report}");
    assert!(!report.contains("the running binds"), "{report}");
}
