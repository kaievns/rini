# Window sizing: defaults per app and display, and why widths reset

Research for Kai's 2026-11-04 ask, "i want to work on default app sizes next", before any spec or
code. Every figure is from 2026-11-04 on main at `eb596883` unless it says otherwise. **Measured**
means a command, a file or the code was read. **Inferred** means reasoned from those. The scratch
scripts the numbers came from were not kept; each table says what it was computed from.

## Verdict

**The resets come from the memory being narrow, not from a missing default, so fix the memory
first (M0).** Launch memory covers one case: an application relaunching into a set of displays it
has already seen. Every other path falls to `default-column-width { proportion 0.5; }`
(`~/.config/rini/config.kdl:127`): a new window of a running app, any app an opening rule matches
(Slack, Outlook, Messages), every remove-and-re-add (undock, replug, leaving native fullscreen),
and every arrangement not seen before. **Then build Model B:** each app class gets one target width
in points, snapped to the nearest preset fraction on the display it opens on. Three numbers
reproduce 18 of Kai's 20 stated (class, display) cells, with no table of display classes. **Short
columns (H1) wait on one trial (E5),** because floating a utility window in a corner already works
and costs no layout code. rini can read a display's physical size: it calls `CGDisplayScreenSize`
for the height today (`src/displays/platform/cursor_warp.rs:448-453`). Model B does not need it.

## Kai's patterns, tested

| # | Reading | Verdict | Evidence |
|---|---|---|---|
| 1 | Width is a function of (app class, display); classes are "work" and "utility" | **Corrected: 3 classes, not 2** | Terminals behave unlike the other work apps, below. Obsidian splits per window |
| 2 | Fractions shrink as displays grow, close to a constant point width snapped to a preset | **Partly holds** | 16 of 20 cells leave-one-out, if the 5K2K runs at 3840x1620. Unknown until E1 |
| 3 | Big displays need columns shorter than the full height | **Holds, inferred** | A 1/4 column on the 32" is 0.45 wide per unit of height. rini cannot shorten a lone window today |
| 4 | Remembered sizes have to work before defaults matter | **Holds, measured** | 5 windows re-maximised twice each on 2026-11-04, each from 860pt |
| 5 | Good built-in defaults, plus rules by app and display | **Holds; the rules need two new keys** | A window rule takes `fixed` only and cannot match a display |

### 1. Three app classes

Source: `~/.rini/layout.ron` `display_memory.launch.apps`, every slot width under all 5
arrangements, focuslab windows excluded. Every slot names the built-in display (failure 6), so all
of them are 16" widths. Effective ratio is 0.5 plus the stored offset.

| Class | Apps | Width records | Distribution (measured) |
|---|---|---|---|
| **Wide** | VS Code, Kiro, Zen, Chrome, Slack, Outlook, Kiro Crew, Shapr3d, ACME | 44 | 43 full, 1 at default |
| **Terminal** | Ghostty | 29 | 1/2 x19, 2/3 x9, 0.533 x1, full x0 |
| **Narrow** | Messages, Books, Finder, Word, Docker, MCS, Connections, Terminal | 22 | all at the default 1/2 |
| Per window | Obsidian | 10 | full x4 (3 are one Excalidraw canvas), default x6 |
| Floating | Zoom, 1Password, System Settings | — | `open-floating` rules, `~/.config/rini/config.kdl:245-276` |

```
16" laptop, records per effective width (non-lab)
1.0   ███████████████████████████████████████████ 43  wide
2/3   █████████ 9                                     terminal
1/2   █████████████████████████████████████████ 41    terminal 19 + narrow 22
```

- **Terminals are their own class.** Kai says terminals go full on the 14", but 0 of 29 Ghostty
  records on the 16" are full. On the 32" he wants a terminal (1/3) narrower than a browser (1/2).
- **A default record is weak evidence.** It also covers windows Kai never touched, so the narrow
  class's 1/2 is not a choice he made.
- **Obsidian needs a title rule,** not an app rule: canvases go full, notes stay at 1/2.
  `reapply-window-rules-on-title-change` exists (`~/.config/rini/config.kdl:224`).

### 2. Constant point width: what the numbers say

A column at ratio r is `r*(V+3) - 3` points, where the viewport V is the usable width less two
3pt gaps (`~/.config/rini/config.kdl:106`). Measured on the 16": 1/3 = 572, 1/2 = 860,
2/3 = 1148, full = 1722, matching `src/layout/domain/constraints.rs:63-70` and the live frames from
`rini-cli query workspaces`.

Displays:

| Display | Looks like | pt/in | Status |
|---|---|---|---|
| 16" built-in | 1728x1117 | 127.5 | measured (`CGDisplayScreenSize`, 344x222mm) |
| 14" MacBook Pro | 1512x982 | 127.0 | assumed default mode; not on this Mac |
| 32" 4K | 3008x1692 | 109.6 | measured: 4 UUIDs at this mode in the window server's display plist |
| 27" 4K | 2560x1440 | 108.8 | assumed; no 4K 27" in the plist |
| 5K2K | 3840x1620 | 104.2 | **assumed: no 21:9 mode in the plist, so this Mac has never driven it** |

Point widths Kai's stated fractions imply (5K2K at 3840):

| Class | 14" | 16" | 27" 4K | 32" 4K | 5K2K | Spread |
|---|---|---|---|---|---|---|
| Editor | 1 = 1506 | 1 = 1722 | — | 1/2 = 1500 | 1/3 = 1276 ("often 1/2") | x1.35 |
| Slack, Outlook | 1 = 1506 | 1 = 1722 | — | 1/2 = 1500 | — | x1.15 |
| Browser | 1 = 1506 | 2/3 = 1147 (records: full) | — | 1/2 = 1500 | 1/3 = 1276 | x1.31 |
| Terminal | 1 = 1506 | 2/3 = 1147 | 1/2 = 1276 | 1/3 = 999 | 1/3 = 1276 | x1.51 |
| Narrow | 1/2 = 752 | 1/2 = 860 | — | 1/4 = 748 | 1/4 = 956 | x1.28 |

Snap test: one target per class, then the nearest preset in {1/4, 1/3, 1/2, 2/3, 1} by log
distance on each display.

| Unit | 5K2K mode | In sample | Leave one out |
|---|---|---|---|
| **points** | **3840** | 19/20 | **16/20** |
| points | 5120 | 14/20 | 14/20 |
| points | 2560 | 12/20 | 12/20 |
| millimetres | any | 18/20 | 15/20 |

- Two of the four leave-one-out misses side with Kai's behaviour over his words: browser at 16"
  (model full, his records full 16 of 16) and editor at the 5K2K (model 1/2, his "often 1/2").
- The two real misses are terminals, at 14" and on the 32". Terminals are the least constant class
  in points and in millimetres.
- **The preset ladder does most of the work.** Its steps (x1.33 to x1.5) are wider than the spread
  within a class (x1.15 to x1.5), so the unit decides only 1 to 4 cells of 20.
- **Points beat millimetres for one reason (inferred):** text is laid out in points, so 1500pt holds
  the same number of characters on every display. Physically that is 299mm on the 16" and 348mm on
  the 32", where the text is 16% larger as well.
- Prior art leans the other way **as a user model**: the two niri users who state Kai's problem
  state it as a fraction per display (n=2, under "Prior art"). Points serve to derive the defaults;
  the presets stay what Kai sees.

### 3. Heights

Column height is the usable height less 6pt. Computed from the displays table.

| Window | Size pt | Width / height | Physical height |
|---|---|---|---|
| 16" narrow at 1/2 | 860x1079 | 0.80 | 215mm |
| 32" narrow at 1/4 | 748x1654 | **0.45** | 383mm |
| 32" narrow at 1/3, half height | 999x825 | 1.21 | 191mm, near the laptop's full height |
| 16" editor full | 1722x1079 | 1.60 | 215mm |
| 32" editor 1/2 | 1500x1654 | 0.91 | 383mm |

What rini does today (measured from code):

- **A lone window cannot be shortened.** A vertical resize on a column of one window returns at once
  (`src/layout/domain/scrolling/width.rs:180-182`), and a lone window's own height report is
  ignored (`height: !self.stacked || …`, `src/layout/domain/tile.rs:110`). So a window rule's
  `default-window-height { fixed N; }` does nothing on a lone tiled window (inferred).
- **Uneven stacks already work.** Fold a window in, then trade height by row weight with
  `set-window-height "±N%"` (`src/layout/domain/scrolling/width.rs:183-197`) or a drag. Kai has no
  height bind.

### 4. The resets are measured

Source: `~/Library/Logs/rini/transitions.jsonl`, 2026-10-05 to 2026-11-04, `press` marks, with the
focuslab batteries of 2026-10-09 to 2026-10-11 excluded.

- **359 width presses in 30 days, about 12 a day:** preset cycle 236, fullscreen-within-gaps 118,
  resize-by 5.
- On 2026-11-04 these windows were maximised twice each with no width press between, each press
  from 860pt (1/2) to 1722pt. The direction is inferred from the order of the frames in each press.

| Window (pid/idx) | Maximised at |
|---|---|
| Outlook 935/104 | 10:01, 13:25 |
| Zen 1274/80 | 12:49, 14:23 |
| VS Code 894/2402 | 09:58, 14:13 |
| VS Code 894/2891 | 14:59, 15:12 |
| Slack 904/30 | 11-03 13:42, 11-04 14:12 |

Only 53 of 564 window births are followed by a width press within 60s, and 226 of the 359 presses
come 10 minutes or more after any birth. Inferred reading: Kai fixes a width when he next focuses
the window, so the journal cannot rank the causes below by frequency.

### 5. What today's config can express

- **No 1/4 width.** `min-column-width { proportion 0.33333; }` (`~/.config/rini/config.kdl:128`)
  and presets 1/3, 1/2, 2/3, 1 (`~/.config/rini/config.kdl:159-164`).
- **No per-display widths.** `output › layout` takes `gaps` and `struts` only
  (`src/app/config/kdl/suggest.rs:67-68`).
- **Per-app widths in points only, with no display match.** A window rule's `default-column-width`
  is a `FixedPart` (`src/app/config/kdl/window_rule.rs:26`, `src/app/config/kdl/suggest.rs:89`).
  It is applied as a resize with no drag (`src/workspaces/engine/rules.rs:231-260`), so it is a
  bounded width and clamped by the min and max (`src/layout/domain/scrolling.rs:1612-1618`). With
  Kai's 1/3 and 2/3, `fixed 1500` on the 16" lands at 1148.
- **Layout-level `fixed` is refused** as "not supported yet"
  (`src/app/config/kdl/suggest.rs:182-185`).

## Why sizes reset today, ranked

The ranking is by reach (how many paths each case covers), since frequency cannot be measured
(pattern 4). Failure numbers are used again under "Model M0".

| # | Failure | When it hits | Code | Evidence | Fix |
|---|---|---|---|---|---|
| 1 | **A new window of a running app gets 1/2** | Cmd+N, a new project window, reopening one window while siblings stay open | Siblings claim slots 0..N-1, so window N has none (`src/workspaces/engine/launch.rs:190-219`); `remember` replaces the set with the live windows, so a closed window's slot goes at the next save (`src/workspaces/domain/launch_memory.rs:154-170`) | By design: "A window beyond the remembered slots gets today's defaults" (`src/workspaces/docs/launch-memory.md:63`). No test covers it (`src/workspaces/engine/persistence/tests/launch_memory.rs` holds relaunch cases only) | Fall back to the app's last width on this display (M0.3); keep closed windows' slots as a capped most-recent list |
| 2 | **Any opening rule turns memory off for its app** | Slack, Outlook, Messages (`~/.config/rini/config.kdl:284-289`, `open-on-output "internal"`), at every relaunch and reboot | Memory is read only when `decision == AppRuleDecision::NoMatch` (`src/workspaces/engine/rules.rs:140`); the rule then assigns a workspace, so the second chance in `add_window_to_layout` (`src/workspaces/engine/discovery.rs:322-338`) is skipped too | Measured in `layout.ron`: Outlook's slots hold full width under 5 of 5 arrangements, written and never read. Outlook appears twice in the reset table above | Apply a rule property by property: only what the rule sets overrides the slot (M0.2) |
| 3 | **A re-added window takes its width from a record only commands write** | Undock, replug, leaving native fullscreen, the window server moving a window between spaces | `WindowRemovedPreserveFloating` records no width (`src/app/reactor/display_homes.rs:358`); the re-add reads `DisplayAffinity::window_width` (`src/workspaces/engine/affinity.rs:181-203`), which only width commands write (`src/workspaces/engine/commands/resize.rs:36,44`, `src/workspaces/engine/commands/floating.rs:67`, `src/workspaces/engine/moves.rs:219`). A drag never writes it (`src/workspaces/engine.rs:647-679`), nor does a rule's resize | Measured in `layout.ron`: the current arrangement's record holds 8 of 14 live windows; every docked arrangement's record is empty. Fits "everything defaults to 1/2 of my laptop screen" after an undock (inferred) | Write the record before every remove-and-re-add and at drag end (M0.1), as `src/workspaces/engine/moves.rs:219` does |
| 4 | **Memory is keyed by the whole set of displays** | Each new desk or meeting room starts empty; the laptop's widths alone do not carry to the same panel docked | `SetupId::of` (`src/workspaces/engine/launch.rs:171`); required by `specs/displays.md:43-49` | Measured: 5 arrangements in `layout.ron` | Fall back through this arrangement, then this display shape, then the default (M0.3) |
| 5 | **Relaunch matching hands out the wrong slot** | Apps with several windows (Ghostty, Chrome, Obsidian) | Title first, against the title at first sighting, then ordinal (`src/workspaces/domain/launch_memory.rs:196-208`) | Measured: 63 of 133 slots (47%) hold no width; 18 slots are focuslab windows (`lab-A` to `lab-D`, opened as Ghostty windows by `tools/focuslab/lab.sh:16-17`) | Drop lab windows from the record; match on the settled title (M0.5) |
| 6 | **The slot's display differs from where the window opens** | Docked only | The width is stored under the slot's display (`src/workspaces/engine/rules.rs:165-167`) and read back under the display of the space the window opened on (`src/workspaces/engine/affinity.rs:189`) | Inferred from code. **Not verified:** 0 of 111 docked slots name an external display | Place the window on the slot's display, or key the stored width by where it lands |
| 7 | Untouched widths are stored as no width | Any window never resized | By design (`src/workspaces/docs/launch-memory.md`, "Recording") | 63 of 133 slots | Moot once class defaults exist |

```
slot widths in ~/.rini/layout.ron (n=133, measured 2026-11-04)
none        ████████████████████████████████ 63
full        ███████████████████████ 46
offset      ████████████ 24
```

**The give-up logs are invisible.** `src/workspaces/docs/launch-memory.md:111-114` says every way
the lookup gives up is logged, but those lines are `debug!`
(`src/workspaces/engine/launch.rs:183,211`) and the service runs at `RUST_LOG=error,warn,info`
(`plutil -p ~/Library/LaunchAgents/git.kaievns.rini.plist`).
`grep -c "remembered\|relaunched" /tmp/rini_kaievns.err.log` gives 0.

**What the specs promise against this:**

- `specs/displays.md:39-42` says a window remembers its column width per arrangement, and
  `specs/displays.md:55-57` includes a drag's unbounded ratio. **Partly met:** a drag reaches launch
  memory through the saved layout, never the record a re-add reads (failure 3).
- `src/workspaces/docs/launch-memory.md:67-73` puts any applying rule above a slot, so a rule that
  only names a display costs the app its width (failure 2). No spec covers placement-only rules.
- A rini restart with the apps alive keeps sizes; that is the one path that works.

## Prior art

| Tool | Width unit | Default keyed on | Per display | App x display | Shorter than full height | Size memory |
|---|---|---|---|---|---|---|
| niri | proportion (gap-aware), fixed logical px | app-id, title | yes, `output { layout {} }` since 25.11 | **no; PR #3474 open** | a lone window with a fixed height sits at the top | none |
| scroll (sway fork) | fraction | `for_window` | yes, `output {}` | not documented | **yes: default height, `align up/down/middle`** | saved layouts |
| hyprscroller (archived) | named fractions | — | yes, `monitor_options` | no | per-monitor default height | none |
| Hyprland scrolling | fraction 0.1 to 1.0 | class | direction only | no | no | floating windows only, per class and title |
| PaperWM | % or px | wm_class, title | no | no | stacks only | none |
| Karousel | % presets | tile or float only | no | no | stacks only | none |
| AeroSpace, yabai, Amethyst | tree ratios, grid | bundle id | gaps only | no | the tree decides | Amethyst layouts |
| Rectangle | screen fractions | one action for all new windows | keep size, or replay the action | no | halves and quarters | none |
| macOS AppKit | points | autosave name | **no: one frame per name (measured, `defaults read com.apple.Terminal`)** | no | the app decides | one frame |
| KWin rules | px | class, title | screen attribute | via rules | n/a | **"Remember" policy** |

- **niri is the closest model.** Per-output `default-column-width` and presets since 25.11
  (https://github.com/niri-wm/niri/blob/main/docs/wiki/Configuration:-Outputs.md). PR #3474
  (https://github.com/niri-wm/niri/pull/3474, open since 2026-02-17, read 2026-11-04) adds
  `match on-output=`. Its motivation is Kai's 32" case: "I want to open a Firefox window at 1/3
  width on the big 32" monitor, but maximized on other screens." The maintainer's stated problem on issue
  #2600 is that `open-on-output` loops with an output match.
- **niri's auto-scale is the only physical-size display class found:** 20" diagonal splits a
  135 dpi target from a 110 dpi one
  (https://github.com/niri-wm/niri/blob/main/src/utils/scale.rs).
- **scroll is the precedent for short columns in the strip:** `layout_default_height` per output
  and `align` for a column in the view or a window in its column
  (https://github.com/dawsers/scroll/blob/master/README.md).
- **niri #1811:** a fixed height equal to the full height looks like "fills the column" but behaves
  differently when a window joins. Short columns need the two kept distinct.
- **Karousel #82** reports a width "reset to some default value" when the display wakes: Kai's
  symptom, in another tool.
- **KWin's policies name Kai's precedence:** *Apply Initially* (a default the user can change),
  *Remember* (the changed value wins next time), *Force*
  (https://userbase.kde.org/KWin_Rules_Window_Attributes).
- **No tool remembers an app's width once its windows close,** and none sizes in physical units.
  rini's per-(window, arrangement) record already goes further than any of them.
- **Tree tilers do not fit:** opening a window reflows its neighbours, which is the tiling Kai wants
  to stay out of.

## Design options

### Model M0: memory that stops resetting (needed under every model)

| Piece | Fixes | Change | Size |
|---|---|---|---|
| M0.1 | 3 | Write the per-window width record before every remove-and-re-add and at drag end | S |
| M0.2 | 2 | A rule overrides a launch slot only for the properties it sets | S |
| M0.3 | 1, 4 | **App width memory keyed by (bundle id, display shape),** written at every width change, read when no per-window record or slot exists | M |
| M0.4 | per-display defaults | Store absolute widths: `ColumnWidth::Offset` is an offset from the configured ratio (`src/layout/domain/column_width.rs:16-17`), so a different default per display would read one record as two widths. Older rini still has to read the file (`specs/displays.md:58-60`) | M |
| M0.5 | 5, logs | Drop focuslab windows from the record; raise the give-up logs to `info` | S |

**Display shape** = usable point width, usable point height, and physical width to the nearest
10mm. The 32" shows under 4 UUIDs at one mode, so a shape key lets same-model desks share widths
(inferred benefit).

**Precedence, in KWin's words (Remember before Apply Initially):**

1. This window's record for this display in this arrangement (exists today:
   `DisplayAffinity::window_width`).
2. The launch slot, on a relaunch into a known arrangement, property by property (M0.2).
3. The width Kai last gave this app on this display shape (M0.3).
4. A rule's default for this app on this display.
5. This app's width on another display shape, carried over (Model B: as points, snapped).
6. The built-in class default.
7. `layout › default-column-width`.

So that an edited rule is not silently beaten by memory, each memory record also stores the
default it overrode, and a record made under an old rule value is dropped when the value changes.

### Models A, B and C

| | A: fractions per display class | **B: point target snapped to presets (recommended)** | C: memory only |
|---|---|---|---|
| Unit | fraction of the viewport | points, resolved to the nearest preset per display | fraction (today) |
| Display classes | named, by point width and aspect | none: the viewport width is the input | display shape, for the memory key |
| Config | class table, per-class `output` layouts, rules with `on-output` | one number per app class | none |
| Unseen display | falls into a class | works with no config | 1/2 until taught |
| Moving between displays | the destination class's default | carried points, snapped again | fraction kept: 860 becomes 1500 (x1.75) |
| Kai's 20 cells | 20/20 (written from them) | 18/20 in sample | n/a |
| niri compatibility | high (PR #3474's shape) | `near` is rini's own unit | n/a |
| Size | M + M | M + S-M | M0 only |

**Why B.** It is the only model that gives a display rini has never seen sensible widths without
config, and it keeps a window the same width in points across displays. A's class table encodes
Kai's 20 answers exactly, but each new display needs a class decision. C leaves defaults at 1/2.

Model B's built-in targets: wide 1500, terminal 1230, narrow 830. Presets {1/4, 1/3, 1/2, 2/3, 1},
dropping any under 560pt on that display. `*` marks cells Kai did not state.

| Class | 14" (V 1506) | 16" (1722) | 27" 4K (2554) | 32" 4K (3002) | 5K2K at 3840 (3834) |
|---|---|---|---|---|---|
| Wide | 1 | 1 | 2/3 * | 1/2 | 1/3 |
| Terminal | 1 (knife edge) | 2/3 | 1/2 | **1/2; Kai said "1/3 maybe"** | 1/3 |
| Narrow | 1/2 | 1/2 | 1/3 * | 1/4 | 1/4 |
| Narrow height (target 830 from {1/3, 1/2, 1}) | full | full | full * | 1/2 | 1/2; Kai said "1/3 tall tops" |

- The other miss is browser at 16", which B puts at full against Kai's 2/3. His records agree
  with B.
- Terminal at 14" decides by 0.002 log units, so the snap needs a tie rule, such as preferring the
  larger preset.
- At 5120x2160 the 5K2K row breaks: narrow 1/4 = 1276pt, wide 1/3 = 1703pt.
- Moving under B: 16" full (1722) lands at 1/2 on the 32"; 32" 1/4 returns to 16" 1/2. Every pair
  computed returns to its start. Precedence step 3 beats the snap, so Ghostty set once to 1/3 on the
  32" stays at 1/3.

Model B in config (proposed, not valid today):

```kdl
layout {
    preset-column-widths { proportion 0.25; proportion 0.33333; proportion 0.5; proportion 0.66667; proportion 1.0; }
    min-column-width { fixed 560; }
    default-column-width { near 1230; }
}
window-rule {
    match app-id=r"^com\.microsoft\.VSCode$"
    match app-id=r"^app\.zen-browser\.zen$"
    default-column-width { near 1500; }
}
window-rule {
    match app-id=r"^com\.apple\.finder$"
    default-column-width { near 830; }
    default-window-height { near 830; }
}
window-rule {
    match app-id=r"^com\.mitchellh\.ghostty$" output-min-width=2800
    default-column-width { proportion 0.33333; }
}
```

The last rule is the escape hatch for one display size. Following niri's lesson, a rule that matches
on the display is matched after `open-on-output` has placed the window, and a rule may not do both.

**Spec changes B and M0 need (Kai's call):**

- `specs/displays.md:43-49`: arrangements share no records. M0.3 shares an app's width across
  arrangements, as a fallback below the per-window record.
- `specs/displays.md:55-57`: a width is never remembered in points. B stores a target in points;
  the reason the line gives (the display size differs) is the case B's snap handles.

### Heights: H1, H2, H3

| | H1: short columns in the strip | H2: floating in a corner | H3: uneven stack as a default |
|---|---|---|---|
| What | a column is `Fill` or `Short { height, align: top, middle, bottom }` | `open-floating` plus `default-floating-position relative-to="top-right"` | a fold-time row weight, such as 3/4 + 1/4 |
| Works today | no: `src/layout/domain/scrolling/width.rs:180` and `src/layout/domain/tile.rs:110` | **yes, with fixed sizes** (`rini.default.kdl:239-245`) | the stack works; the default does not exist |
| Cost | L, plus M for a right-corner anchor: the offset clamp never shows space before the first column (`src/layout/docs/strip.md:492-495`) | S for proportional sizes | S |
| Against | breaks "a column fills the height" in the overview, `Mod+F`, folds, edge drags; fixtures under `SLIDE_SCORES` and `ZOOM_SCORES` may move (inferred) | the window leaves the strip, so focus, column moves and the overview treat it differently | only applies once Kai folds a window in: a new window always opens as its own column (`specs/strip.md:12-13`) |

H1 would bring in niri's names rini refuses today: `preset-window-heights`
(`src/app/config/kdl/suggest.rs:178`), an absolute `set-window-height "50%"`, and a window rule's
`default-window-height { proportion 0.5; }`.

## Experiments

Undo any of them with `target/release/rini-cli execute config reload`. A `window-rule` in a fragment
replaces the whole running list (`specs/config.md:92-94`), so each fragment carries Kai's 6 running
rules verbatim from `rini-cli execute config get`, followed by the new rules below. The fragments
were not applied or validated; `config set` refuses one it cannot use and changes nothing.

**Cost of every rule trial:** a rule on an app turns launch memory off for it (failure 2), so
VS Code, Zen, Chrome and Ghostty lose relaunch memory while E2 or E3 runs.

**Baseline:** journal line 507423 of `~/Library/Logs/rini/transitions.jsonl` (2026-11-04). Count
width presses since then with:

```sh
tail -n +507424 ~/Library/Logs/rini/transitions.jsonl \
  | grep -cE '"mark":"press","what":"(cycle_preset_column_width|toggle_fullscreen_within_gaps|resize_window_by)"'
```

| # | Where, how long | Change | What to look for |
|---|---|---|---|
| E1 | At home, 5 min | `rini-cli query displays` and `system_profiler SPDisplaysDataType \| grep -E "Resolution\|UI Looks like"` | The 5K2K's looks-like width. It decides points against millimetres, and B's 5K2K row |
| E2 | 16" alone, 2 days | `max-column-width` to 1.0; wide apps `fixed 1722`, Ghostty `fixed 1148` | Width presses on fresh windows. A bounded 1.0 column is not `Mod+F`'s mode; what `Mod+F` does on it is unchecked. Do not dock with it |
| E3 | 32" docked, 3 days | `min-column-width` 0.25, 1/4 added to the presets; wide apps `fixed 1500`, Ghostty `fixed 999`, Finder and Books `fixed 748` | Is 1/4 too narrow, or only too tall? On day 2 set Ghostty to 1500 to compare a 1/2 terminal. Windows opened on the laptop get clamped values |
| E4 | 32" docked, 1 day | Bind `Mod+Shift+Equal` and `Mod+Shift+Minus` to `set-window-height "+5%"` and `"-5%"` (both unbound) | VS Code at 1/2, Ghostty folded under with `Mod+Comma`, 5 presses to 25%. Does 3/4 + 1/4 beat 1/2 + 1/3 side by side? |
| E5 | 32" docked, 2 days | Finder: `open-floating true`, `default-floating-position x=3 y=3 relative-to="top-right"`, `fixed 748` by `fixed 825` | Is 1/4 by 1/2 the right shape, and does leaving the strip hurt? Decides H1 against H2 |
| E6 | Code (S-M), then 1 week on both setups | `near N` in a window rule's `default-column-width`: snap to the nearest preset after the min and max clamp | B in miniature: one fragment serves the laptop and the 32", which `fixed` cannot |
| E7 | After M0, 1 week | Re-run the count | Width presses a day against about 12. A repeat `Mod+F` on a window already full is near 0 |

E2's new rules (E3 and E5 follow the same shape with the values above):

```kdl
layout { max-column-width { proportion 1.0; } }
window-rule {
    match app-id=r"^com\.microsoft\.VSCode$"
    match app-id=r"^dev\.kiro\.desktop$"
    match app-id=r"^com\.amazon\.kiro\.crew$"
    match app-id=r"^app\.zen-browser\.zen$"
    match app-id=r"^com\.google\.Chrome$"
    match app-id=r"^com\.tinyspeck\.slackmacgap$"
    match app-id=r"^com\.microsoft\.Outlook$"
    exclude is-modal=true
    default-column-width { fixed 1722; }
}
window-rule {
    match app-id=r"^com\.mitchellh\.ghostty$"
    exclude is-modal=true
    default-column-width { fixed 1148; }
}
```

## Open questions for Kai

1. **Does the 5K2K run at 3840x1620 looks-like?** Yes keeps B's 5K2K row; no means E1 first.
2. **May an app's width carry across arrangements, below the per-window record (M0.3)?** It amends
   `specs/displays.md:43-49`. Yes or no.
3. **Is a utility window floating in a corner (H2) acceptable, or must it stay in the strip (H1)?**
   E5 is the trial; answer after it.

## Needs checking

- Why no docked slot names the 32". Next time docked, compare each window's `home_display` in
  `rini-cli query diagnostics` with the display it sits on.
- Saved launch-slot offsets are not shifted when `default-column-width` changes live, while live
  columns are (inferred). Do not change it during the trials.
- What `move-window-up` and `move-window-down` do on a lone window, which bears on H1's alignment
  keys.

## Side quests (parked)

- Stored bounded offsets run past the clamp: Ghostty `Offset(1.36667)` and `Offset(4.76667)` in
  `layout.ron` against a 0.66667 max. They clamp when drawn
  (`src/layout/domain/constraints.rs:63-70`); where they come from is unexplained.
- `resize_selection_by` with nothing selected rewrites the strip's `column_width_ratio`
  (`src/layout/domain/scrolling/width.rs:166-172`).
- Autosave is not held back while displays churn (`src/app/reactor/autosave.rs:53-63`), so a save
  mid-change can file one arrangement's windows under another's key (inferred, low impact).
