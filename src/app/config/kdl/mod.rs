//! The file format: KDL v1, decoded with knus into one part type per section, niri's shape, every
//! field optional, and then laid over a `Config` by `apply`. Where else the format reaches is
//! `docs/config.md`.

pub mod apply;
pub mod bar;
pub mod binds;
pub mod error;
pub mod gestures;
pub mod input;
pub mod layout;
pub mod suggest;
pub mod value;
pub mod window_rule;

use knus::ast::SpannedNode;
use knus::errors::DecodeError;
use knus::span::Span;
use knus::traits::Decode;

use self::bar::{BarPart, ConfigNotificationPart, RecentWindowsPart};
use self::binds::BindsPart;
use self::error::{ConfigError, Problem};
use self::gestures::{AnimationsPart, GesturesPart};
use self::input::{CursorPart, InputPart};
use self::layout::{LayoutPart, OutputPart};
use self::value::Flag;
use self::window_rule::{SpawnPart, SpawnShPart, WindowRulePart, WorkspacePart};
use crate::app::startup::StartupCommand;

/// The `kind` a bind's decoder reports an action name rini does not have under. Its message is the
/// name alone, so the report can say what was probably meant.
pub const UNKNOWN_ACTION: &str = "action";

/// One config text, decoded. Every section is optional, and a singleton written twice is an error.
#[derive(Debug, Default)]
pub struct ConfigPart {
    pub input: Option<InputPart>,
    pub cursor: Option<CursorPart>,
    pub layout: Option<LayoutPart>,
    pub animations: Option<AnimationsPart>,
    pub gestures: Option<GesturesPart>,
    pub recent_windows: Option<RecentWindowsPart>,
    pub bar: Option<BarPart>,
    pub config_notification: Option<ConfigNotificationPart>,
    pub binds: Option<BindsPart>,
    pub activate_spaces_by_default: Option<Flag>,
    pub reapply_window_rules_on_title_change: Option<Flag>,
    /// The repeatable nodes, each in file order.
    pub outputs: Vec<OutputPart>,
    pub workspaces: Vec<WorkspacePart>,
    pub window_rules: Vec<WindowRulePart>,
    pub startup: Vec<StartupCommand>,
    pub undecoded: Undecoded,
}

/// What the nodes that could not be decoded leave unknown. A check across nodes that needs it is
/// skipped: run on what was there before, it would report problems the text does not have.
#[derive(Debug, Default)]
pub struct Undecoded {
    /// An `input` failed or was written twice, so `Mod` may stand for another modifier.
    pub mod_key: bool,
    /// A `workspace` failed, so the names are not all known.
    pub workspace_names: bool,
    /// A `window-rule` failed, so the list that replaces the running rules is not all known.
    pub window_rules: bool,
    /// A `binds` or `recent-windows` failed or was written twice, so which running binds the text
    /// replaces is not known.
    pub binds: bool,
}

/// Decodes `text` as far as it goes, with every problem found on the way. Only a syntax error
/// stops it; a node that cannot be decoded is left out of the part. `name` is what a report calls
/// the text.
pub fn parse(name: &str, text: &str) -> Result<(ConfigPart, Vec<Problem>), ConfigError> {
    let document = knus::parse_ast::<Span>(name, text).map_err(|error| {
        let problems = miette::Diagnostic::related(&error)
            .map(|related| related.map(Problem::from_diagnostic).collect())
            .unwrap_or_default();
        ConfigError::new(name, text, problems)
    })?;

    let unknown = suggest::unknown_nodes(&document);
    let mut decoding = Decoding::default();
    let mut part = ConfigPart::default();
    for node in &document.nodes {
        let d = &mut decoding;
        match &**node.node_name {
            "input" => d.once(&mut part.input, node),
            "cursor" => d.once(&mut part.cursor, node),
            "layout" => d.once(&mut part.layout, node),
            "animations" => d.once(&mut part.animations, node),
            "gestures" => d.once(&mut part.gestures, node),
            "recent-windows" => d.once(&mut part.recent_windows, node),
            "bar" => d.once(&mut part.bar, node),
            "config-notification" => d.once(&mut part.config_notification, node),
            "binds" => d.once(&mut part.binds, node),
            "activate-spaces-by-default" => d.once(&mut part.activate_spaces_by_default, node),
            "reapply-window-rules-on-title-change" => {
                d.once(&mut part.reapply_window_rules_on_title_change, node)
            }
            "output" => part.outputs.extend(d.decode(node)),
            "workspace" => part.workspaces.extend(d.decode(node)),
            "window-rule" => part.window_rules.extend(d.decode(node)),
            "spawn-at-startup" => {
                if let Some(SpawnPart { span, argv }) = d.decode(node) {
                    if argv.is_empty() {
                        d.problems.push(Problem::at(
                            &span,
                            "`spawn-at-startup` needs a program",
                            "nothing to run",
                        ));
                    } else {
                        part.startup.push(StartupCommand::Spawn(argv));
                    }
                }
            }
            "spawn-sh-at-startup" => {
                if let Some(SpawnShPart { command }) = d.decode(node) {
                    part.startup.push(StartupCommand::SpawnSh(command));
                }
            }
            _ => {}
        }
    }
    let switcher = part.recent_windows.as_mut().and_then(|recent| recent.binds.as_mut());
    for block in part.binds.iter_mut().chain(switcher) {
        decoding.errors.append(&mut block.errors);
    }
    part.undecoded = Undecoded {
        mod_key: decoding.failed.contains(&"input"),
        workspace_names: decoding.failed.contains(&"workspace"),
        window_rules: decoding.failed.contains(&"window-rule"),
        binds: decoding.failed.iter().any(|name| ["binds", "recent-windows"].contains(name)),
    };

    // knus reports a node it does not know where the node starts, as an unexpected node in a struct
    // or a failed conversion in an enum; the walk has reported those already, with a suggestion.
    let reported: Vec<usize> = unknown.iter().map(|(offset, _)| *offset).collect();
    let mut problems: Vec<Problem> = unknown.into_iter().map(|(_, problem)| problem).collect();
    problems.extend(decoding.problems);
    problems.extend(decoding.errors.into_iter().filter_map(|error| match error {
        DecodeError::Unexpected { span, kind: "node", .. }
        | DecodeError::Conversion { span, .. }
            if reported.contains(&span.0) =>
        {
            None
        }
        DecodeError::Unexpected {
            span,
            kind: UNKNOWN_ACTION,
            message,
        } => Some(suggest::unknown_action(&message, &span)),
        other => Some(Problem::from(other)),
    }));
    Ok((part, problems))
}

#[derive(Default)]
struct Decoding<'a> {
    errors: Vec<DecodeError<Span>>,
    problems: Vec<Problem>,
    first: Vec<(&'a str, Span)>,
    /// The nodes, by name, left out of the part.
    failed: Vec<&'a str>,
}

impl<'a> Decoding<'a> {
    fn decode<T: Decode<Span>>(&mut self, node: &'a SpannedNode<Span>) -> Option<T> {
        knus::decode::node::<T, Span>(node)
            .map_err(|errors| {
                self.errors.extend(errors);
                self.failed.push(&**node.node_name);
            })
            .ok()
    }

    /// A section that may be written once.
    fn once<T: Decode<Span>>(&mut self, slot: &mut Option<T>, node: &'a SpannedNode<Span>) {
        let name = &**node.node_name;
        let here = *node.node_name.span();
        if let Some((_, first)) = self.first.iter().find(|(seen, _)| *seen == name) {
            self.problems.push(
                Problem::at(&here, format!("`{name}` is written twice"), "written again here")
                    .also(first, "first written here")
                    .help("a section is written once; put everything in the first one"),
            );
            self.failed.push(name);
            return;
        }
        self.first.push((name, here));
        *slot = self.decode(node);
    }
}
