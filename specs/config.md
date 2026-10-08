# The config file

> **Reported 2026-10-01.** "i want you to switch toml based configuration to KDL and adopt similar (or
> the same) structure as niri does". The TOML file was rift's format, grown by accretion: four
> required-or-not tables, a command vocabulary that was the wire protocol's serde names, bindings in
> hash order, two settings whose default depended on whether their table was written, validation that
> never ran at boot, and reloads that failed silently. Kai's decisions on the design are recorded
> beside the requirements they settled.

## The format

- rini MUST read its config from `~/.config/rini/config.kdl`, or the path `--config` names, written in
  KDL v1, the version niri reads.
- Wherever rini and niri share a concept, the config MUST use niri's section names, node names,
  nesting and value forms, so what a niri snippet for a shared section (`layout`, `binds`,
  `window-rule`, `workspace`) says about a shared concept means the same in rini. What the snippet
  names that rini lacks MUST be refused with the reason (below), so a snippet need not paste in whole.
  A concept niri does not have MAY have a name of rini's own, in the section niri would put it in.
- Every section MUST be optional. A setting left out keeps its default; binds and window rules are the
  exception, and a file that writes none has none, as in niri. So is `layout › border`'s `on`: a
  `border` block that writes neither `on` nor `off` turns the border on, as niri's does.
- An empty section MUST mean exactly what an absent one does, but for `layout › border`, whose empty
  block `border {}` turns the border on, as above. Two TOML settings broke this: a workspace swipe
  skipped empty workspaces when its table was absent and not when it was empty, and the drag-swap
  overlap was 0 with no table, 0.3 with one, and 0.5 after any reload.

> **Reported 2026-10-06.** Kai's decision D2: a `layout › border` with neither `on` nor `off` means
> on, exactly as niri. niri reads it so because its default config's wording led users to write
> `border {}` to turn borders on (`niri:niri-config/src/lib.rs:253-271`), so a niri `border` block
> pasted in means here what it means there.
- Order MUST be the file's: binds, window rules and workspaces keep the order they are written in.
- The reference, `rini.default.kdl`, MUST mean the same settings and workspaces as an empty file, so
  it cannot drift from what rini does when a setting is left out.

## When a file cannot be used

- A name rini does not know MUST be refused, naming the line. When a known name is near, the report
  MUST say which; when the name is niri's and rini does not have it, the report MUST say why rather
  than offer a lookalike.
- Every problem in a file MUST be reported in one report, each with its line and column: what does
  not decode and what the checks across nodes find (a key bound twice, a workspace that is not
  declared, widths that do not nest) alike. Two kinds MAY show only once another problem is fixed.
  One is a second problem inside one node: decoding a node stops at its first bad child or value, as
  niri's does, though every bind is a node of its own. The other is a check across nodes that needs a
  node that did not decode, which would otherwise report a problem the file does not have.
- A section that may be written once, written twice, and a key bound twice, MUST be refused with both
  places named.
- A number out of range MUST be refused where it is written, at launch as well as on a reload.
- A config that cannot be used at launch MUST NOT stop rini starting: it starts on the built-in
  defaults and reports why (`specs/persistence.md`).
- A missing `config.kdl` means the built-in defaults, with nothing to report, unless a `config.toml`
  sits beside it. Then the launch MUST report it as a config that cannot be used, naming both files,
  and `rini --validate` MUST fail: the defaults are not what that file configured.
- A reload that cannot be used, whether the file changed, `load-config-file` was pressed or
  `rini-cli execute config reload` asked, MUST keep the running config and report why in the log.
  `rini-cli` MUST get the report as its error.
- While the file cannot be used, at launch or on a reload, every bar MUST be marked until the file is
  read and can be used; `config-notification { disable-failed; }` turns the mark off
  (`specs/bar.md`).

> **Reported 2026-10-01.** Kai's decision on the switch: a failed reload shows a marker in rini's bar
> until the next good load, plus the log, with niri's `config-notification { disable-failed; }` to
> turn it off. A reload that failed used to be dropped without a word, so a typo looked like "my
> change did nothing".

- `rini --validate` MUST check the config, printing `config is valid: PATH` or the report and exiting 1,
  before it checks the saved layout.

## Window rules

- A `window-rule` MUST apply to a window that matches ANY of its `match` lines and NONE of its
  `exclude` lines; a rule with no `match` line MUST apply to every window. Every matcher on one line
  MUST hold.
- Every rule that applies MUST be applied, in file order, a later one overriding an earlier one
  property by property.
- `app-id`, `title` and `app-name` MUST be regular expressions, case-sensitive and unanchored, as
  niri's are. An invalid one MUST be refused, naming the line.
- `is-active`, `is-focused` and `is-floating` MUST match a window by whether it holds focus and
  whether it floats. What a window opens with reads it as it opens, holding no focus and not
  floating, as niri reads a new window; what a rule says a window wears follows it while it is open
  (`decorations.md`).
- Modal dialogs MUST float by an ordinary rule in `rini.default.kdl`, so a later rule can decide
  otherwise for the windows it matches.

> **Reported 2026-10-01.** Kai's decision on the switch: niri's rule semantics, rather than rini's
> single most specific rule with exact, case-insensitive app ids. Under rini's semantics a pasted niri
> rule failed in one of two ways: a second `match` line was an error, or a regex app id was compared as
> a literal bundle id and silently never matched. The one change Kai's own rules see is the Outlook
> reminder window, which one rule floats and another pins to the internal display: it now gets both.

## Changing the running config

- `rini-cli execute config set '<KDL>'` MUST apply the fragment over the running config: a section's
  settings replace the running ones, binds replace the binds for their keys, and `workspace` or
  `window-rule` nodes replace the running list. A fragment that cannot be used MUST change nothing.
  It lasts until the next reload; the file is the config.
- The config a fragment leaves MUST pass every check a file does. A fragment that changes `mod-key`
  so two running binds become one key, or replaces the workspaces so a running bind or rule names one
  that is gone, MUST be refused, naming them.
- `rini-cli execute config get` MUST print the running config.

## Where it lives

`src/app/config/` reads and applies the file: `kdl/` decodes each section and `kdl/apply.rs` lays it
over the settings; `kdl/suggest.rs` holds the two name tables. The decisions behind the format are
`docs/config.md`.
