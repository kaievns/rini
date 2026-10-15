# The config format

rini's config is `~/.config/rini/config.kdl`, in niri's shape: KDL v1, niri's sections and node
names wherever the two share a concept, rini's own names where they do not. `rini.default.kdl` is
the annotated reference; the requirements are `specs/config.md`. This file is the decisions behind
the format, and every place rini knowingly differs from niri.

## Parser: knus, KDL v1

- **knus** decodes the file. It is the maintained fork of knuffel, the crate niri parses its config
  with, and keeps knuffel's `#[derive(Decode)]` model, so rini's part types are shaped as niri's are.
  Every error carries a source span and miette renders the report.
- **KDL v1**, because that is the syntax niri ships, documents and its users paste. Of 254 `kdl`
  blocks in niri's wiki and default config, every one parses as v1 and 70 fail as v2: bare
  `true`/`false`, `r"…"` raw strings, bare `null`.
- **Pinned to a commit, not the 3.4.0 release.** knus-derive 3.4.0 depends on `proc-macro-error2`,
  whose future-incompatibility report cargo printed on every build. Commit `20eb6f7` ("Drop proc
  macro error2 (#57)", 2026-09-02) removes it and no release carries it yet. Move to the release
  that does.
- **v2 later**, by niri's route: parse v1 or v2 with kdl-rs, convert to v1, decode with knus.

## How a file becomes a config

1. `kdl::parse` reads the text into knus's AST. A syntax error stops here.
2. The schema walk (`kdl/suggest.rs`) checks every node name against `SCHEMA`, the names each
   section takes, and reports an unknown one with a suggestion, or with niri's reason from `NIRI`
   when the name is niri's. knus's own report of the same node, an unexpected node in a struct or a
   failed conversion in an enum, is matched by where the node starts and dropped.
3. Each section decodes into a part type (`kdl/{input,layout,gestures,bar,binds,window_rule}.rs`)
   whose every field is optional. Ranges are checked here, with the span. knus keeps no value of a
   node with an error anywhere in it and stops a struct at its first bad child, as niri's knuffel
   does, so a section with a problem is left out of the part. A bind block is the exception: it
   decodes each bind on its own and carries their errors, so one bad bind leaves the rest.
4. `kdl/apply.rs` lays whatever decoded over a `Config`, field by field, and runs the checks that
   need more than one node on the config that results: widths that nest, one `focus-at-startup`,
   unique workspace names, workspace names that exist, keys bound once. A check that needs a node
   that did not decode is skipped (`Undecoded`): with an `input` left out, a clash counts only if it
   holds whatever `mod-key` says; with a `workspace` left out, no name is checked; with a
   `window-rule` left out, no running rule is checked, and with `binds` or `recent-windows` left
   out, no running bind is.
5. Both lists of problems go into one report, in file order.

Loading the file is `apply(parse(file), code defaults)`; `config set` is
`apply(parse(fragment), running config)`. The defaults are each feature's `Default` impl and nothing
else, which is why an empty section cannot mean something different from an absent one: an absent
part and an empty part both leave every field alone. Because the checks run on the result, a running
bind or rule a fragment does not mention is still checked against the `mod-key` and workspaces the
fragment leaves.

The format lives in `app/config/kdl`, except for what a bind names. `input::domain::binding` derives
knus's `Decode` for `Action` and decodes the values its actions take: `Decimal`,
`WorkspaceReference` and `MonitorReference` as knus scalars, and `SizeChange`, `SwitcherScope` and
`SwitcherFilter` from the config's spelling with `FromStr`, as `rini-ipc` does for `RestoreScope`
and `RestoreSource`. The features' settings types carry no parsing.

## Naming

| Rule | Why |
|---|---|
| A shared concept takes niri's exact node name, nesting and value form | niri snippets and muscle memory carry over |
| A rini-only concept is a kebab-case node in the section niri would use | `input` for focus and the pointer, `layout` for the strip, `gestures` for drag, the top level for whole-WM flags, as niri's `prefer-no-csd` |
| A bare flag turns something on, and flags are named for the non-default behaviour | niri's convention; a flag also takes `true` or `false` |
| An unknown node, property or argument is an error | niri's behaviour; the TOML silently accepted typos in one table |
| Indices start at 1 | niri's `focus-workspace 1` is the first workspace (Kai's decision, `specs/input.md`) |

The two pointer flags niri has off by default, `focus-follows-mouse` and `warp-mouse-to-focus`,
take niri's default: off. They were on, and a default-on flag can only be written as
`x false`, which no niri config contains.

## Where rini differs from niri

| Node | rini | Why |
|---|---|---|
| `input › mod-key` | `"Alt"` unless written | on macOS Cmd, niri's `Super`, belongs to the applications |
| `animations › slowdown` | the one movement's length is 0.35s times `slowdown` | rini has one movement animation; niri scales each animation's own base |
| `layout › preset-column-widths` | the default adds `proportion 1.0` | 1.0 is the whole viewport as a width `max-column-width` does not hold, never the maximise mode, so a maximised column can be stepped back down (`src/layout/docs/strip.md`) |
| `layout › center-focused-column "always"` | the first and last columns sit flush with the display edge | there is nothing beyond them to scroll to; niri centres them. `"on-overflow"` is refused, not yet supported |
| `layout › focus-ring` | as niri, on unless `off`, but 2pt wide by default; `on` and `off` both written is refused | Kai's width, niri's is 4 (`specs/decorations.md`); niri takes `on` of the pair, and every rini block with both refuses it. `urgent-color` and `urgent-gradient` paint every window of an application asking for attention, not the one window niri marks, since macOS asks per application; a gradient's `relative-to="workspace-view"` is refused, not yet supported |
| `layout › border` | as niri: off unless written, and a block writing neither `on` nor `off` turns it on; a tiled window gives the border its width out of its place in the strip; a floating window keeps its size and wears the border outside it; `on` and `off` both written is refused | niri grows a floating tile by its border instead, and rini does not resize a window the user or the app placed (`specs/decorations.md`). `urgent-color` and `urgent-gradient` as in `focus-ring`; a gradient's `relative-to="workspace-view"` is refused, not yet supported |
| `layout › gaps`, `struts` | as niri: `gaps` between windows and at the edges, a strut adds to one edge; but a strut is 0 or more | one gap value, so rini's separate horizontal and vertical inner gaps are gone. niri's negative struts are refused, not yet supported |
| `output "UUID"` | a display UUID, and only `layout › gaps` and `struts` | niri names outputs by connector or make, model and serial, so a niri name such as `"eDP-1"` is taken as a UUID and matches no display; macOS owns the rest of what niri puts in `output` |
| `window-rule › open-on-output` | `"internal"` or `"external"`, a ROLE | a UUID differs between the monitor at home and the one at the office; a pin is a default, not a law (`specs/displays.md`) |
| `window-rule › open-on-workspace` | a name only | as niri |
| `window-rule › default-floating-position` | logical pixels from that corner of the display | as niri; rini's old form was a fraction of the screen |
| `window-rule › default-column-width`, `default-window-height` | `fixed` only | a floating window opens at it; a tiled one is resized once |
| `window-rule › open-focused` | `true` only | rini cannot stop a new window taking focus yet |
| `window-rule › match` | adds `app-name`, `ax-role`, `ax-subrole`, `is-modal` | macOS's names for what niri reads from Wayland |
| `window-rule › match` with no matcher | refused | niri matches every window with it; leaving the line out already does that |
| `window-rule › match is-active`, `is-focused`, `is-floating`, `is-urgent` | as niri, but `is-active` and `is-focused` match the same window, the one holding focus, and `is-urgent` every window of an application asking for attention while none of its windows holds focus; at open a window holds no focus, does not float and is not urgent, so a rule matching any of them sets nothing a window opens with | niri's `is-active` matches each workspace's active window on the focused monitor, every one whose ring it paints active; rini paints those rings active too, but a rini display shows one workspace at rest, so `is-active` matches the window holding focus, and a ring drawn active on another window in flight takes the active paint the rules give that one. `is-active-in-column` is refused: focus entering a rini column lands on the row it came from, so no window of the column is its own. macOS asks for attention per application, not per window |
| `window-rule › focus-ring`, `border` | as niri: laid over `layout`'s in file order, a later `on` or `off` beating an earlier one; a rule's block turns nothing on by being written; `on` and `off` both written in one rule is refused | niri takes `on` of the pair (`specs/decorations.md`). Under `is-active`, `is-focused` or `is-urgent` only the colours and gradients are taken: a border's `on`, `off` and `width` are refused because a border that changed with focus or attention would lay the strip out again every time either changed, and a ring's are refused, not yet supported |
| `window-rule › geometry-corner-radius` | as niri without `clip-to-geometry`: the ring and the border round to it, ahead of the radius the window server reports; one radius, or four that are equal | the hairline, the shadow and a clipped picture keep the window's own corner, since on a tile they draw what macOS draws at rest (`specs/decorations.md`), so `clip-to-geometry` stays refused. Unequal corners are refused, not yet supported: a band rounds each corner alike. Under `is-active`, `is-focused` or `is-urgent` it is refused, not yet supported: a tile's corner is laid when it is staged, and only its paint turns during a flight |
| `window-rule › draw-border-with-background` | `false` only | rini draws the ring and the border hollow; `true` would need a fill under another application's window, not yet supported |
| a `window-rule` that sets only `focus-ring`, `border` or `geometry-corner-radius` | does not apply to a window as it opens | rini opens a window where its application's windows last were only when no rule applies at open, which niri has no form of; a rule about what a window wears must not take that away |
| `spawn`, `spawn-at-startup` | the program as written: a leading `~` is not expanded | niri expands it; not yet supported, so write the full path |
| `workspace "name" { focus-at-startup; }` | the workspace shown at launch | niri's word, from `output`; rini's workspaces span every display |
| `binds` | a held key fires once; `repeat=true` opts in | Kai's decision: a repeat re-fired commands such as workspace back-and-forth |
| `binds` keys | macOS's keys under niri's names; `Super` is Cmd, and a side can be named (`LAlt`, `RCmd`) | niri's mouse, wheel, touchpad and tablet triggers are refused, not yet supported, and `XF86` media keys never reach the tap as keys |
| `recent-windows › binds` | `next-window` with neither `filter` nor `scope="workspace"` steps between applications, each drawn as a stack of its windows | Kai's ask (`specs/switcher.md`): one entry per window was too long to use. niri's switcher is one entry per window |
| `recent-windows › binds` | `scope="workspace"` spans every display | a rini workspace is one context across displays; `scope="output"` is refused |
| `recent-windows › binds` | no defaults | niri's default, Alt+Tab, is `Mod+Tab` under rini's default `mod-key`, which is `focus-workspace-previous`; and Cmd+Tab belongs to the Dock |
| `config-notification { disable-failed; }` | turns off the bar's `✕ config` marker | niri shows an on-screen notification; rini's bar is where it says things |
| `animations › journal` | rini's own: every transition's steps are written to `~/Library/Logs/rini/transitions.jsonl`, off unless written | niri has no journal. Off by default, on where it is wanted for tracing (Kai's ask of 2026-10-05, `specs/animation.md`), so the bare flag names the non-default |
| `animations › journal { measure-handover; }` | rini's own: each lift is held up to 60ms for the overlay's own picture and measured against the screen just after it, into the journal; `journal false` turns it off with the writing | niri has no journal. Off by default because it holds every lift (`src/transitions/docs/journal.md`, "The handover, measured") |
| `quit` | saves the layout and exits, never asks | so `skip-confirmation=` is refused |
| `load-config-file` | bindable | IPC-only in niri |
| `focus-window-or-monitor-down`, `-up` | the only vertical focus | at the column's end it crosses to the display below; niri's `focus-window-down` would not, so it is not offered |
| `set-column-width "+5%"` | relative percentages only | niri's absolute `"50%"` and pixel forms are refused, not yet supported |

rini-only actions keep rini's own words: `move-window-left` and `-right` (a lone window swaps
columns, a stacked one leaves as a new column, which niri's `move-column-left` never does),
`toggle-fold-left` and `-right` (niri's `consume-or-expel-window-left` never round-tripped:
`src/layout/docs/strip.md`), `toggle-stack`, `cycle-visible-windows`,
`snap-strip`, `create-workspace`, `move-mouse-to-monitor-…`, `switch-space-left` and `-right`,
`toggle-space-activated`, the layout save and restore, and the debug actions. Every action and the
wire command it lowers to is the table in `src/app/hotkeys/lower/tests.rs`.

## What went with the TOML

| What | Why |
|---|---|
| `hot_reload` | the file is always re-read, and a failed reload keeps the running config, so there is nothing to guard |
| `virtual_workspaces.enabled` | nothing read it |
| `default_workspace_count` | the `workspace` nodes are the count; "Workspace N" padding went with it |
| `prevent_wrapping` | workspace stepping never wraps, as niri's `focus-workspace-up` and `-down` do not |
| `modifier_combinations` | `Mod` with `mod-key` covers it: Kai's `comb1` is `Mod+Shift`, `comb2` is `Mod+Alt` |
| anchored `left` and `right` alignment | not expressible in niri's vocabulary, and no config used it |
| separate horizontal and vertical inner gaps | niri has one `gaps` |
| `exec`'s string form, split on single spaces, and the startup commands' own quote-aware splitter | `spawn` takes argv as written and `spawn-sh` hands one string to `sh -c` |
| `title_substring`, `app_name`'s substring either way, a rule's workspace index, a fractional position | niri's regex matchers and pixel position replace them |
| `float_modal_windows` | the modal rule is an ordinary `window-rule` in `rini.default.kdl` |
| `Meta` as a modifier name | niri's `Super`, or `Cmd` |
| `swap_windows`, `focus_window`, and window ids in binds | a config cannot know a window id; they are IPC only, as niri's `#[knuffel(skip)]` actions |
| `config save` | it rewrote the file without its comments. The file is the config |
| the typed `config set-…` setters and the JSON-path `config set` | `config set '<KDL>'` applies a fragment through the same code as the file |
| the legacy bare `resize_window_grow` migration | it existed for TOML, and made every parse error lose its span |

A recording (`rini --record`) no longer carries the config, since a config has no written form but
its file; `replay` takes one.
