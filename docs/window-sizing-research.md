# Window sizing: defaults per app and display, and why widths reset

Research for Kai's 2026-11-04 ask, "i want to work on default app sizes next", before any spec or
code. Every figure is from 2026-11-04 on main at `eb596883` unless it says otherwise. **Measured**
means a command, a file or the code was read. **Inferred** means reasoned from those. The parse
scripts, their per-slot, per-day and per-reset outputs and the Model B fit are in
`docs/data/2026-11-04-window-sizing/`, whose README names each file; each table names its source
and how it was counted. Kai answered the research's three questions the same day ("Kai's answers"),
and every figure they touch is recomputed with them: the 5K2K at 3360x1418 points, and the two
1920x1200 displays discarded. No launch slot names either of them, so every slot is still counted.
Line numbers in `src/` are at `eb596883`, before M0; "After M0" says which failures M0 fixed and
which remain.

## Verdict

**The resets come from the memory, not from a missing default, so fix the memory first (M0).**
The strongest pattern in the logs is waking from sleep (failure 8): on 2026-11-04, 7 of 14 width
presses came within 3 minutes after a wake, and one VS Code window was full when the display went
off and back at 1/2 when Kai re-maximised it 80 seconds after the wake. The mechanism is not
verified. Beyond that, launch memory covers one case: an application relaunching into a set of
displays it has already seen. Every other path falls to `default-column-width { proportion 0.5; }`
(`~/.config/rini/config.kdl:127`): a new window of a running app, any app an opening rule matches
(Slack, Outlook, Messages), a window the window server moves between spaces, and every arrangement
not seen before. **Then build Model B:** each app class gets one target width in points, snapped
to the nearest preset fraction on the display it opens on. With the 5K2K at 3360x1418, three
numbers reproduce 16 of Kai's 20 stated (class, display) cells, with no table of display classes;
two rules keyed on display width bring it to 18, and the last 2 side with his records and his
"often 1/2". **Utility windows on big displays float in a corner (H2), Kai's choice;** short
columns in the strip (H1) are deferred. rini can read a display's physical size:
`physical_height_mm` calls `CGDisplayScreenSize` today
(`src/displays/platform/cursor_warp.rs:455-458`). Model B does not need it.

**M0 is built:** failures 8, 3, 2 and 1 are fixed in the reactor's and the engine's tests, failure
4 for displays of one shape, and the focuslab half of failure 5. Open: failure 5's title matching,
failures 6 and 7, and every piece of Model B and H2 ("After M0").

## Kai's patterns, tested

| # | Reading | Verdict | Evidence |
|---|---|---|---|
| 1 | Width is a function of (app class, display); classes are "work" and "utility" | **Corrected: 3 classes, not 2** | Terminals behave unlike the other work apps, below. Obsidian splits per window |
| 2 | Fractions shrink as displays grow, close to a constant point width snapped to a preset | **Partly holds** | 18 of 20 cells in sample, 15 leave-one-out, with the 5K2K at 3360x1418. Its 1/3 (1116pt) is narrower than the 16"'s 2/3 (1147pt) |
| 3 | Big displays need columns shorter than the full height | **Holds, inferred; met by floating (H2)** | A 1/4 column on the 32" is 0.45 wide per unit of height. rini cannot shorten a lone window today, and Kai accepts a floating corner window instead |
| 4 | Remembered sizes have to work before defaults matter | **Holds, measured** | 5 windows reset twice each on 11-03 and 11-04, each from 860pt; 11 resets tied to their log lines in §4 |
| 5 | Good built-in defaults, plus rules by app and display | **Holds; the rules need two new keys** | A window rule takes `fixed` only and cannot match a display |

Kai's words this research tests, beyond the table above (2026-11-04):

- Narrow apps "1/2 on 16" and probably 14" too, smaller screens, i dunno maybe 2/3".
- On the 32", narrow windows at "1/3-1/4", and "too tall for those too".
- "shrink a window to 1/2 by 1/2 and move it in a workspace to any of the four corners wether we
  have or don't have other windows on that strip".
- "i hate moving windows between screens and watching them taking completely different sizes".

### 1. Three app classes

Source: `~/.rini/layout.ron` `display_memory.launch.apps`, parsed to
`docs/data/2026-11-04-window-sizing/slots.csv` and counted by `recount.py` there. The file holds 133
slots under 5 arrangements. 111 name the built-in display (37D8832A…): 29 under the laptop alone
and 82 under the 3 docked arrangements. The other 22 name C5A3728A…, a display that formed an
arrangement on its own. It has no `display_space` entry and no layout size in `layout.ron`, so its
size is unknown, and those slots are left out below. No slot names a 1920x1200 display
(64F4B5B7…, 71A9490F…). Two docked arrangements include one, but their 50 slots all name the
built-in display, so they are 16" widths and stay in. The table counts the 111 built-in slots less
12 focuslab windows, so every width is a 16" width. Effective ratio is 0.5 plus the stored offset,
clamped to 2/3 unless the slot is `width_dragged`.

| Class | Apps | Width records | Distribution (measured) |
|---|---|---|---|
| **Wide** | VS Code, Kiro, Zen, Chrome, Slack, Outlook, Kiro Crew, Shapr3d, ACME | 38 | 37 full (Slack's one is a dragged 1.0), 1 at default |
| **Terminal** | Ghostty | 23 | 1/2 x15 (13 default), 2/3 x7, 0.533 x1, full x0 |
| **Narrow** | Messages, Books, Finder, Word, Docker, MCS, Connections, Terminal | 21 | all at the default 1/2 |
| Per window | Obsidian | 8 | full x3 (2 are one Excalidraw canvas, 1 a note), default x5 (1 the same canvas) |
| Floating | Zoom, 1Password, System Settings | 8 | `open-floating` rules, `~/.config/rini/config.kdl:245-276` |

Preview's one record, at 2/3, is in no class.

```
16" laptop, records per effective width (built-in slots, non-lab)
1.0   █████████████████████████████████████ 37  wide
2/3   ███████ 7                                 terminal
1/2   ████████████████████████████████████ 36   terminal 15 + narrow 21
```

- **Terminals are their own class.** Kai says terminals go full on the 14", but 0 of 23 Ghostty
  records on the 16" are full. On the 32" he wants a terminal (1/3) narrower than a browser (1/2).
- **A default record is weak evidence.** It also covers windows Kai never touched, so the narrow
  class's 1/2 is not a choice he made.
- **Obsidian needs a title rule,** not an app rule: the canvas went full in 2 of its 3 records,
  notes stayed at 1/2 in 4 of 5. The canvas's 2 full records are the laptop's, docked to a
  1920x1200 display, so they are 16" evidence like the rest. A title rule would need
  `reapply-window-rules-on-title-change` (`~/.config/rini/config.kdl:224`), and a rule's width
  resize runs "once, after it joins the strip" (`rini.default.kdl:243-245`); whether a title
  change runs it again is unchecked.

### 2. Constant point width: what the numbers say

A column at ratio r is `r*(V+3) - 3` points, where the viewport V is the usable width less two
3pt gaps (`~/.config/rini/config.kdl:106`). Measured on the 16": 1/3 = 572, 1/2 = 860,
2/3 = 1148, full = 1722, matching `src/layout/domain/constraints.rs:63-70` and the live frames from
`rini-cli query workspaces`.

Displays:

| Display | Looks like | pt/in | Status |
|---|---|---|---|
| 16" built-in | 1728x1117 | 127.6 | measured (`CGDisplayScreenSize`, 344x222mm) |
| 14" MacBook Pro | 1512x982 | 127.0 | assumed default mode; not on this Mac |
| 32" 4K | 3008x1692 | 109.6 | measured: 4 UUIDs at this mode (14 entries) in the window server's display plist |
| 27" 4K | 2560x1440 | 108.8 | assumed; the plist's 14 entries at 2560x1440 are all 1x, so not a 4K panel |
| 5K2K | 3360x1418 | **unknown** | **Kai, 2026-11-04: "it runs on 3360x1418".** No 21:9 mode in the plist, so its physical size is not on this Mac: 107.3 pt/in if a 34" panel, 91.2 if a 40" one |

The plist counts are from `plutil -convert json` of
`/Library/Preferences/com.apple.windowserver.displays.plist`, every mode with `Wide`, `High` and
`Scale`. The two 1920x1200 displays in `layout.ron` (64F4B5B7…, 71A9490F…) are discarded, as Kai
asked, and appear in no table or fit; the laptop's slots while docked to them are counted in §1.

Point widths Kai's stated fractions imply (`fit.py`, "point widths"):

| Class | 14" | 16" | 27" 4K | 32" 4K | 5K2K | Spread |
|---|---|---|---|---|---|---|
| Editor | 1 = 1506 | 1 = 1722 | — | 1/2 = 1500 | 1/3 = 1116 ("often 1/2") | x1.54 |
| Slack, Outlook | 1 = 1506 | 1 = 1722 | — | 1/2 = 1500, **unreachable** | — | x1.15 |
| Browser | 1 = 1506 | 2/3 = 1147 (records: full) | — | 1/2 = 1500 | 1/3 = 1116 | x1.35 |
| Terminal | 1 = 1506 | 2/3 = 1147 | 1/2 = 1276 | 1/3 = 999 | 1/3 = 1116 | x1.51 |
| Narrow | 1/2 = 752 | 1/2 = 860 | — | 1/4 = 748 (Kai: "1/3-1/4") | 1/4 = 836 | x1.15 |

- **Slack and Outlook on the 32" cannot happen today.** Kai's comms rule pins Slack, Outlook and
  Messages to the internal display (`~/.config/rini/config.kdl:284-289`, `open-on-output
  "internal"`). The cell counts in the snap test but no trial can reach it while the pin holds.
- **Narrow on the 32" is a range.** The snap test takes 1/4. 1/3 is 999pt.

Snap test: one target per stated class (editor, browser, Slack and Outlook, terminal, narrow), the
geometric mean of its cells, then the nearest preset in {1/4, 1/3, 1/2, 2/3, 1} by log distance on
each display, dropping any under 560pt (`fit.py`, "snap test"). Leave one out takes the target
from the class's other cells.

| Unit | 5K2K | In sample | Leave one out |
|---|---|---|---|
| **points** | **3360x1418 (Kai)** | 18/20 | **15/20** |
| millimetres | 34" panel, assumed (796mm) | 18/20 | 16/20 |
| millimetres | 40" panel, assumed (936mm) | 18/20 | 15/20 |

- Points miss 5 cells leave-one-out. One sides with Kai's hedge over his words: editor at the 5K2K
  (model 1/2, his "often 1/2"). The other 4 are real: editor at 16" (model 2/3), browser at the
  5K2K (1/2 against 1/3), and terminals at 14" and on the 32".
- **The 5K2K's 1/3 (1116pt) is narrower than the 16"'s 2/3 (1147pt).** At 3840 it was 1276pt. So
  a class that is full on the 16" and 1/3 on the 5K2K, as Kai's editor and browser are, has no
  constant point width: full on the 16" needs a target above 1405pt, 1/3 on the 5K2K one below
  1368pt.
- **The preset ladder does most of the work.** Its steps (x1.33 to x1.5) are as wide as the spread
  within a class (x1.15 to x1.54), and every unit scores 18 of 20 in sample.
- **The unit no longer decides the fit.** Millimetres score 15 or 16 leave-one-out, by the 5K2K's
  panel size, which is unknown. Points are kept for two reasons. Text is laid out in points, so
  1500pt holds the same number of characters on every display (inferred); physically that is 299mm
  on the 16" and 348mm on the 32", where the text is 16% larger as well. And a point target needs
  no physical size, and the 5K2K's is not known.
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
| 5K2K narrow at 1/4 | 836x1380 | 0.61 | unknown |
| 5K2K narrow at 1/4, half height | 836x688 | 1.22 | unknown |
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

Source: `~/Library/Logs/rini/transitions.jsonl`, 2026-10-05 to 2026-11-04, `press` marks, each
bucketed by local day from the `unix_ms` of the burst mark before it. Width presses are the
preset cycle, the full-width toggle and resize-by: the three `what` values the count command under
"Experiments" matches.

```
width presses per journaled day (n=359)
10-06  ███ 3
10-07  ████ 4
10-08  ██████████████ 14
10-09  ████████████████████████████████████████████████████████████████████████████████ 98   lab
10-10  ████████████████████████████████████████████████████████████████████████████████ 133  lab
10-11  ████████████████████████████████████████████████████████████████████████████████ 83   lab
10-19  █ 1
11-02  ████████ 8
11-03  █ 1
11-04  ██████████████ 14
```

- **314 of 359 presses fall on the focuslab days, 10-09 to 10-11.** focuslab posts full-width
  presses ("full width `3:a`", `tools/focuslab/README.md:203`).
- **Outside them: 45 width presses over 16 journaled days, about 3 a day, and 14 on 11-04.** The
  16 days are 10-05 to 10-08, 10-12 to 10-20 and 11-02 to 11-04; the journal holds no burst
  between 10-21 and 11-01.
- Drags are left out: a `mouse_up` press marks every click and drag end alike (233 in the whole
  journal), so it cannot be counted as a resize.
- Outside the lab days, 36 of 377 birth events are followed by a width press within 60s, and 17 of
  the 45 presses come 10 minutes or more after any birth. A birth event is one `window_created`
  (56), `windows_discovered` (207) or `application_launched` (114) mark; the last counts apps, not
  windows. The journal alone cannot rank the causes below by frequency, so each reset is tied to
  the service log instead.

**Each reset, tied to its log line.** Sources: `/tmp/rini_kaievns.err.log` (UTC, local is +11h),
user wakes from `pmset -g log`, app start times from `ps -o lstart`, the reboot from
`last reboot`. Window identity is from the order of the frames in each press in the journal. The
widths at display-off are from the `ApplyOverlayFrames` line of the layout pass rini runs as the
display turns off.

| Window (pid/idx) | Reset (local) | err.log | Last user wake before | What precedes it | Failure |
|---|---|---|---|---|---|
| Slack 904/30 | 11-03 13:42:37 | :39 | reboot 13:38 | Slack launched 13:39:57; rini loaded the layout at 13:40:38 with `saved_windows=8, restore_candidates=4` (:1) | 2, fits (inferred) |
| not identified | 11-04 09:27:46 | :182 | 09:26:44, 1:02 before | — | 8, timing only |
| VS Code 894/2402 | 09:58:27 | :286 | 09:26:44, 31 min | `MainWindowChanged on unknown window` for pid 894 at 09:58:12 (:257) | 1 (inferred: a window rini had not seen) |
| Outlook 935/104 | 10:01:18 | :363 | 09:26:44, 35 min | nothing logged for 935/104 | **open** |
| Zen 1274/80 | 12:49:20 | :650 | 12:38:30, 11 min | re-add "window moved to the space the window server has it on … from=None" at 12:49:17 (:646) | 3, measured |
| Outlook 935/104 | 13:25:43 | :683 | 13:22:02, 3:41 | nothing logged for 935/104 | 8, timing only |
| Slack 904/30 | 14:12:20, a preset cycle | :751 | 14:11:35, 0:45 | already 860 at the 13:32:54 display-off pass (:727), before the sleep | **open**, not a wake reset |
| VS Code 894/2402 | 14:13:18 | :859 | 14:11:35, 1:43 | — | 8, timing only |
| Zen 1274/80 | 14:23:32, a cycle then `Mod+F` (:957) | :952 | 14:11:35, 12 min | re-added `from=None` again at 13:00:55 (:662, :666), laid at 860 at 13:32:54 (:727) | **open**: record erased or not read |
| VS Code 894/2891 | 14:59:45 | :1100 | 14:11:35, 48 min | `MainWindowChanged on unknown window` for pid 894 at 14:59:18 (:1086) | 1 (inferred) |
| VS Code 894/2891 | 15:12:44 | :1259 | 15:11:24, 1:20 | **laid at 1722 at the 15:08:52 display-off pass (:1216)** | 8, measured at both ends |

- **Outlook cannot be failure 2 on 11-04.** Outlook pid 935 started 2026-11-03 13:39:57 and ran
  all day, and failure 2 bites only at a launch.
- **Zen's second reset is not failure 3 as written.** The 12:49 `Mod+F` wrote `FullWidth` to the
  record (`src/workspaces/engine/commands/floating.rs:67`), yet the 13:00:55 re-add laid Zen at
  860. So the record was erased or not read, not never written.
- **A wake does not reset every window.** Outlook and Zen were full at the 15:08:52 pass (:1216)
  and drew no width press after the 15:11:24 wake; their width after it is not logged.
- Against chance: 7 of 14 width presses on 11-04 came within 3 minutes after one of 9 user wakes,
  and 61 of 253 other user presses did (focus, workspace, fold, switch, mouse). Width presses are
  twice as likely there, on a sample of 14.

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

Failure 8 leads because it has the most resets behind it (4 of 11 in §4, one measured at both
ends). The rest are ranked by reach (how many paths each case covers). Failure numbers are used
again under "Model M0".

| # | Failure | When it hits | Code | Evidence | Fix |
|---|---|---|---|---|---|
| 8 | **A width is lost across sleep and wake** | Display off, sleep, wake; seen on the laptop alone | **Mechanism not verified.** Candidate (a): a window briefly not `EffectivelyManageable` takes `LayoutEvent::WindowRemoved`, not the preserving variant (`src/app/reactor/display_homes.rs:340`, `:417`). That runs `forget_persisted_window`, then `memory.affinity.forget_window` (`src/workspaces/engine/persistence/mod.rs:168-171`), which erases the width record (`src/workspaces/domain/display_setup.rs:144-147`). A re-add that logs `from=None` fits it: only a window with no workspace gives `None` (`src/app/reactor/space_affinity.rs:145-150`). Candidate (b), weaker: the `open-on-output` pin is re-applied on every settled topology (`src/workspaces/docs/workspaces-and-displays.md:110-111`), which moves homes, not widths | 4 of 11 resets in §4. VS Code 894/2891 full at the 15:08:52 display-off pass and reset 80s after the next wake. At 13:32:54, as the display turned off, `MainWindowChanged on unknown window` came for Chrome, Obsidian, Outlook and loginwindow (pid 431) in one pass (err.log:722-733). Karousel #82 reports the same symptom | Log every record erase at `info` with its caller (M0.5), reproduce with one sleep, then keep the record across a remove that is not a close (M0.6) |
| 1 | **A new window of a running app gets 1/2** | Cmd+N, a new project window, reopening one window while siblings stay open | Siblings claim slots 0..N-1, so window N has none (`src/workspaces/engine/launch.rs:190-219`); `remember` replaces the set with the live windows, so a closed window's slot goes at the next save (`src/workspaces/domain/launch_memory.rs:154-170`) | By design: "A window beyond the remembered slots gets today's defaults" (`src/workspaces/docs/launch-memory.md:63`). No test covers it (`src/workspaces/engine/persistence/tests/launch_memory.rs` holds relaunch cases only). Fits VS Code 2402 at 09:58 and 2891 at 14:59 (inferred) | Fall back to the app's last width on this display (M0.3); keep closed windows' slots as a capped most-recent list |
| 2 | **Any opening rule turns memory off for its app** | Slack, Outlook, Messages (`~/.config/rini/config.kdl:284-289`, `open-on-output "internal"`), at every relaunch and reboot | Memory is read only when `decision == AppRuleDecision::NoMatch` (`src/workspaces/engine/rules.rs:140`); the rule then assigns a workspace, so the second chance in `add_window_to_layout` (`src/workspaces/engine/discovery.rs:322-338`) is skipped too | Measured in `layout.ron`: Outlook's slots hold full width under 5 of 5 arrangements, written and never read. Slack's 11-03 13:42 reset comes 2 minutes after a cold start (err.log:1, :39) | Apply a rule property by property: only what the rule sets overrides the slot (M0.2) |
| 3 | **A re-added window takes its width from a record only commands write** | Undock, replug, leaving native fullscreen, the window server moving a window between spaces | `WindowRemovedPreserveFloating` records no width (`src/app/reactor/display_homes.rs:358`); the re-add reads `DisplayAffinity::window_width` (`src/workspaces/engine/affinity.rs:181-203`), which only width commands write (`src/workspaces/engine/commands/resize.rs:36,44`, `src/workspaces/engine/commands/floating.rs:67`, `src/workspaces/engine/moves.rs:219`). A drag never writes it (`src/workspaces/engine.rs:647-679`), nor does a rule's resize | Measured for Zen at 12:49 (err.log:646, 3s before the press). The current arrangement's record holds 8 of 14 live windows. **Not a test of undocking:** the docked arrangements' records are empty, but closed windows' records are dropped on every settled topology (`src/workspaces/docs/workspaces-and-displays.md:94-99`), the 11-03 13:38 reboot closed every window, and err.log names only `ScreenId(1)` since | Write the record before every remove-and-re-add and at drag end (M0.1), as `src/workspaces/engine/moves.rs:219` does |
| 4 | **Memory is keyed by the whole set of displays** | Each new desk or meeting room starts empty; the laptop's widths alone do not carry to the same panel docked | `SetupId::of` (`src/workspaces/engine/launch.rs:171`); `specs/displays.md`, "One work context, arranged differently per display", keeps a window's records apart and, since Kai's answer 2, allows an app's width to carry | Measured: 5 arrangements in `layout.ron` | Fall back through this arrangement, then this display shape, then the default (M0.3) |
| 5 | **Relaunch matching hands out the wrong slot** | Apps with several windows (Ghostty, Chrome, Obsidian) | Title first, against the title at first sighting, then ordinal (`src/workspaces/domain/launch_memory.rs:196-208`) | Measured: 63 of 133 slots (47%) hold no width; 18 are focuslab windows (`lab-A` to `lab-D`, opened as Ghostty windows by `tools/focuslab/lab.sh:16-17`), 12 on the built-in display and 6 under C5A3728A… | Drop lab windows from the record; match on the settled title (M0.5) |
| 6 | **The slot's display differs from where the window opens** | Docked only | The width is stored under the slot's display (`src/workspaces/engine/rules.rs:165-167`) and read back under the display of the space the window opened on (`src/workspaces/engine/affinity.rs:189`) | Inferred from code. **Not verified:** 0 of 82 docked slots name an external display | Place the window on the slot's display, or key the stored width by where it lands |
| 7 | Untouched widths are stored as no width | Any window never resized | By design (`src/workspaces/docs/launch-memory.md`, "Recording") | 63 of 133 slots | Moot once class defaults exist |

```
slot widths in ~/.rini/layout.ron (n=133, measured 2026-11-04)
none        ███████████████████████████████▌ 63
full        ███████████████████████ 46
offset      ████████████ 24
```

**The give-up logs were invisible at `eb596883`; M0 raised them to `info`.**
`src/workspaces/docs/launch-memory.md` said every way
the lookup gives up is logged, but those lines are `debug!`
(`src/workspaces/engine/launch.rs:183,211`) and the service runs at `RUST_LOG=error,warn,info`
(`plutil -p ~/Library/LaunchAgents/git.kaievns.rini.plist`).
`grep -c "remembered\|relaunched" /tmp/rini_kaievns.err.log` gives 0. Nor is a remove at
`src/app/reactor/display_homes.rs:340` or `:417` logged, which is why failure 8's mechanism cannot
be read from the log.

**What the specs promise against this:**

- `specs/displays.md` says a window remembers its column width per arrangement, a drag's
  unbounded ratio included. **Partly met at `eb596883`, met after M0:** a drag reached launch
  memory through the saved layout, never the record a re-add reads (failure 3).
- `specs/displays.md` already backs per-display widths: "a browser be full-width when the laptop is
  alone and half-width on the external".
- `src/workspaces/docs/launch-memory.md` put any applying rule above a slot, so a rule that only
  names a display cost the app its width (failure 2), and no spec covered placement-only rules.
  After M0, `specs/workspaces.md`, "Relaunching an application", does.
- `specs/displays.md` requires, from Kai's 2026-11-04 report, that a width the user set, by a
  command or by dragging an edge, survive the display sleeping and waking, the window server
  removing and re-adding the window, a drag of the window, and a relaunch of an app an opening rule
  matches, when the rule sets no width or has not changed it since. M0 meets it under a rule that
  sets no width but for failures 5 and 6 at a relaunch. Under a rule that sets a width, the rule's
  width still wins at a relaunch, changed or not (`specs/workspaces.md`, "Relaunching an
  application", not yet met).
- `specs/displays.md`, "A rule's width is a window's first", makes a rule's changed width show once
  on the windows it sizes, and `specs/workspaces.md` makes a rule's width win over a launch slot's.
  M0 meets both but for a rule edited while its window was out of every strip, or while rini was
  not running.
- `specs/displays.md` requires a new window of a running app to open at the width the user last
  gave that app on a display of the same shape. M0 meets it (failure 1).
- A rini restart with the apps alive, at 21:28 UTC on 2026-11-04 after `f401aef8` was deployed,
  laid every strip at the width the saved layout held, checked after the deploy for VS Code, Slack,
  Outlook, Zen, Ghostty and Messages. It also erased 7 of those live windows' width records before
  their apps reported them (`specs/displays.md`, Found 2026-11-05). The records now survive in tests
  (`a_live_window_s_records_survive_a_settled_topology_before_its_app_reports_it`,
  `src/app/reactor/tests/widths.rs`); not verified on a restart.

### After M0

M0 is M0.1 to M0.6 below, built and merged after this research. M0.6 was built before the one
sleep M0.5's logs were to measure: every removal but a close now keeps a window's widths, so the
erase behind candidate (a) of failure 8 no longer happens on any removal path. Whether a wake takes
that path is still not verified. Each test named is the one that pins the fix; none was run
against the live service.

| # | After M0 | Held by |
|---|---|---|
| 8 | **Fixed in tests, not verified on a wake.** Only a close, the app quitting or the window being gone erases a window's widths; a minimize, a window briefly not manageable, one left out of a snapshot or moved to another space keep them. Every erase is logged at `info` with why | `a_full_width_window_keeps_its_width_across_the_display_sleeping_and_waking` (`src/app/reactor/tests/widths.rs`), `a_window_turned_unmanageable_or_minimized_keeps_its_width` and `every_erase_of_a_width_record_says_why_at_info` (`src/app/reactor/tests/displays.rs`), `a_removal_erases_widths_only_when_the_window_closed` (`src/workspaces/domain/display_affinity.rs`) |
| 1 | **Fixed.** A new window of a running app opens at the width the user last gave the app on a display of that shape, by a width command or a drag | `a_new_window_of_a_running_app_opens_at_the_width_its_app_last_had`, `a_new_window_of_a_running_app_opens_at_the_width_a_drag_gave_its_app` (`src/app/reactor/tests/widths.rs`) |
| 2 | **Fixed.** A rule overrides a launch slot only for what it sets, so Kai's `open-on-output "internal"` rules keep the slot's workspace and width. A rule's width still beats the slot's, even where the user widened the window since (`specs/workspaces.md`, "Relaunching an application", not yet met) | `a_rule_that_only_pins_a_display_keeps_the_launch_slot_s_workspace_and_width` (`src/workspaces/engine.rs`), `a_rule_sets_of_a_launch_slot_only_what_it_names` (`src/workspaces/domain/app_rules.rs`) |
| 3 | **Fixed.** A width only the layout holds, as a drag's, is recorded before a removal, and a re-added window, or one back from native fullscreen under another id, is laid at it | `a_window_the_window_server_removes_and_re_adds_keeps_its_width`, `a_width_set_by_a_drag_is_recorded_and_survives_the_window_being_taken_away_and_given_back`, `a_width_only_the_layout_holds_survives_the_window_server_moving_the_window_away_and_back` (`src/app/reactor/tests/widths.rs`), `a_window_back_from_fullscreen_under_another_id_is_laid_at_the_width_its_old_id_had` (`src/app/reactor/tests/fullscreen.rs`) |
| 4 | **Fixed for a display of the same shape** (usable points and physical width to 10mm). Open: a display of another shape opens at the default, and an existing window moved onto a display it has no width on takes the default there (`specs/displays.md`, not yet met) | `an_app_opened_at_a_desk_not_seen_before_on_a_display_of_the_same_shape_takes_its_width`, `an_app_opened_on_a_display_of_another_shape_opens_at_the_default` (`src/workspaces/engine/app_widths/tests.rs`) |
| 5 | **Half fixed.** focuslab windows leave no slot and take none, and a file's lab slots are dropped as it loads. **Open:** a relaunched window's title is matched as first seen, before its app settles it | `a_focuslab_window_leaves_no_slot`, `a_file_with_focuslab_slots_loads_without_them_and_writes_what_an_older_rini_reads` (`src/workspaces/domain/launch_memory.rs`) |
| 6 | **Open.** A slot's width is still written under the slot's display and read under the window's (`src/workspaces/docs/launch-memory.md`, "Not met") | — |
| 7 | **Open,** by design until a default per class of app exists (Model B) | — |

- **The give-up logs are at `info`,** so the service's log names every way a window takes no
  launch slot: `a_window_that_takes_no_remembered_slot_says_why_at_info`
  (`src/app/reactor/tests/workspaces.rs`).
- **Only an application's width is absolute (M0.4):**
  `a_width_is_stored_as_a_fraction_of_the_display_not_an_offset`
  (`src/workspaces/domain/app_widths.rs`). A window's records and launch slots are still offsets
  from `default-column-width`.
- **Of the 3 unexplained resets in §4,** Zen's at 14:23, a record written by `Mod+F` and laid at
  860 after a re-add, fits an erase at a removal, which M0 no longer makes (inferred). Outlook's at
  10:01 and Slack's preset cycle at 14:12 stay unexplained; the `info` logs name the erase or the
  give-up if either comes again.

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
  width on the big 32" monitor, but maximized on other screens." The maintainer's stated problem on
  issue #2600, "Window rules per output OR window matching by output"
  (https://github.com/niri-wm/niri/issues/2600), is that `open-on-output` loops with an output
  match.
- **niri's auto-scale is the only physical-size display class found:** 20" diagonal splits a
  135 dpi target from a 110 dpi one
  (https://github.com/niri-wm/niri/blob/main/src/utils/scale.rs).
- **scroll is the precedent for short columns in the strip:** `layout_default_height` per output
  and `align` for a column in the view or a window in its column
  (https://github.com/dawsers/scroll/blob/master/README.md).
- **niri #1811** (https://github.com/niri-wm/niri/issues/1811): a fixed height equal to the full
  height looks like "fills the column" but behaves differently when a window joins. Short columns
  need the two kept distinct.
- **Karousel #82, "[Bug] Windows change their width when display turns on"**
  (https://github.com/peterfajdiga/karousel/issues/82), reports a width "reset to some default
  value" when the display wakes: Kai's symptom and failure 8, in another tool.
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
| M0.2 | 2 | A rule overrides a launch slot only for the properties it sets (`specs/workspaces.md`, "Relaunching an application"). Its width winning only once the value changed since the slot was written is not built | S |
| M0.3 | 1, 4 | **App width memory keyed by (bundle id, display shape),** written at every width change, read when no per-window record or slot exists. `specs/displays.md` requires it for a new window of a running app (failure 1) and allows it across arrangements since Kai's answer 2 (failure 4) | M |
| M0.4 | per-display defaults | Store absolute widths: `ColumnWidth::Offset` is an offset from the configured ratio (`src/layout/domain/column_width.rs:16-17`), so a different default per display would read one record as two widths. Older rini still has to read the file (`specs/displays.md`) | M |
| M0.5 | 5, 8, logs | Drop focuslab windows from the record; raise the give-up logs to `info`; log every width-record erase with its caller | S |
| M0.6 | 8 | Once M0.5 names the path: keep the width record across a remove that is not a close, such as a window that is not `EffectivelyManageable` for a moment | S-M, after the measurement |

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
default it overrode, and a record made under an old rule value is not read once the value changes.
M0 stores only the `default-column-width` an application's width overrode, and keeps a record made
under another default unread rather than dropping it (`specs/displays.md`, "One work context,
arranged differently per display"). It stores no rule value, so a rule's width beats a launch
slot's (`src/workspaces/docs/launch-memory.md`, "Precedence").

### Models A, B and C

| | A: fractions per display class | **B: point target snapped to presets (recommended)** | C: memory only |
|---|---|---|---|
| Unit | fraction of the viewport | points, resolved to the nearest preset per display | fraction (today) |
| Display classes | named, by point width and aspect | none: the viewport width is the input | display shape, for the memory key |
| Config | class table, per-class `output` layouts, rules with `on-output` | one number per app class | none |
| Unseen display | falls into a class | works with no config | 1/2 until taught |
| Moving between displays | the destination class's default | carried points, snapped again | fraction kept: 860 becomes 1500 (x1.75) |
| Kai's 20 cells | 20/20 (written from them) | 16/20; 18/20 with 2 display rules | n/a |
| niri compatibility | high (PR #3474's shape) | `near` is rini's own unit | n/a |
| Size | M + M | M + S-M | M0 only |

**Why B.** It is the only model that gives a display rini has never seen sensible widths without
config, and it keeps a window the same width in points across displays, which answers "i hate
moving windows between screens and watching them taking completely different sizes". A's class
table encodes Kai's 20 answers exactly, but each new display needs a class decision. C leaves
defaults at 1/2.

Model B's built-in targets: wide 1500, terminal 1230, narrow 830. Presets {1/4, 1/3, 1/2, 2/3, 1},
dropping any under 560pt on that display. `*` marks cells Kai did not state. Source: `fit.py`,
"Model B".

| Class | 14" (V 1506) | 16" (1722) | 27" 4K (2554) | 32" 4K (3002) | 5K2K at 3360 (3354) |
|---|---|---|---|---|---|
| Wide | 1 | 1 | 2/3 * | 1/2 | **1/2; Kai said 1/3, and "often 1/2" for the editor** |
| Terminal | 1 (knife edge) | 2/3 | 1/2 | **1/2; Kai said "1/3 maybe"** | 1/3 |
| Narrow | 1/2 | 1/2 | 1/3 * | 1/4 (inside Kai's "1/3-1/4") | 1/4 |
| Narrow height (target 830 from {1/3, 1/2, 1}) | full | full | 1/2 * | 1/2 | 1/2; Kai said "1/3 tall tops" |

```
Kai's stated cells reproduced, of 20 (fit.py)
A, written from the cells             ████████████████████ 20
B + 2 display-width rules             ██████████████████ 18
best 3 targets, VS Code 2/3 on 16"    █████████████████ 17
B, 3 targets                          ████████████████ 16
```

- **The 4 misses:** editor at the 5K2K (B 1/2, Kai 1/3 with "often 1/2"), browser at the 5K2K
  (B 1/2, Kai 1/3), terminal on the 32" (B 1/2, Kai "1/3 maybe"), and browser at 16" (B full, Kai
  2/3). His records side with B on the last: 9 of 9 built-in Chrome slots and 4 of 4 Zen are
  full.
- **No other three targets do better without taking the laptop's wide apps off full.** The best three (wide 1230 to
  1360pt) reach 17/20, but put VS Code, Slack and Outlook at 2/3 on the 16", against 37 of 38 wide
  records at full there. Splitting browsers from the wide class reaches 18/20 with four targets, at
  the cost of browsers at 2/3 on the 16", against the same 13 records.
- **Two rules keyed on display width reach 18/20 instead:** browsers at 1/3 on displays at least
  3200pt wide, Ghostty at 1/3 on displays at least 2800pt wide (below). The last 2 misses are the
  editor at the 5K2K, where B gives Kai's "often 1/2", and the browser at 16", where B gives his
  records.
- **A fifth miss if Kai means it:** "smaller screens, i dunno maybe 2/3" for narrow apps. On a 13"
  at 1470 looks-like (assumed mode, V 1464), B gives narrow 1/2 (730pt against 2/3's 975pt).
- Terminal at 14" decides by 0.002 log units, so the snap needs a tie rule, such as preferring the
  larger preset.
- **Moving under B does not always come back.** Carried as points to another display and back, 22
  of 88 (preset, display, display) trips land on another preset (`fit.py`, "moving"). Each starts
  at a width with no near equal on the other display: 2/3 and full on the 32" both land full on the
  16", and a full 16" window lands at 1/2 on the 32". Precedence step 3 is read before any carry, so
  a display Kai has set a width on gets that width back, and Ghostty set once to 1/3 on the 32"
  stays at 1/3.

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
window-rule {
    match app-id=r"^app\.zen-browser\.zen$" output-min-width=3200
    default-column-width { proportion 0.33333; }
}
```

The last two rules are the escape hatch for one display size. Following niri's lesson, a rule that matches
on the display is matched after `open-on-output` has placed the window, and a rule may not do both.

**Spec changes B and M0 need:**

- Made: `specs/displays.md` now keeps a window's records apart and allows an app's width to
  carry across arrangements below them (M0.3), on Kai's answer 2.
- Kai's call: `specs/displays.md`, a width is never remembered in points. B stores a target in
  points; the reason the line gives (the display size differs) is the case B's snap handles.

### Heights: H1, H2, H3

**H2 is the chosen path for utility windows on big displays, and H1 is deferred** (Kai's answer 3,
2026-11-04: a utility window floating in a corner is acceptable). H1's cost below stays as the
record of why it waits. H3 is unchanged: it needs no decision, since the stack already works.

| | H1: short columns in the strip (**deferred**) | H2: floating in a corner (**chosen**) | H3: uneven stack as a default |
|---|---|---|---|
| What | a column is `Fill` or `Short { height, align: top, middle, bottom }` | `open-floating` plus `default-floating-position relative-to="top-right"` | a fold-time row weight, such as 3/4 + 1/4 |
| Works today | no: `src/layout/domain/scrolling/width.rs:180` and `src/layout/domain/tile.rs:110` | **yes, with fixed sizes** (`rini.default.kdl:239-245`) | the stack works; the default does not exist |
| Cost | L, plus M for a right-corner anchor: the offset clamp never shows space before the first column (`src/layout/docs/strip.md:492-495`) | S for proportional sizes | S |
| Against | breaks "a column fills the height" in the overview, `Mod+F`, folds, edge drags; fixtures under `SLIDE_SCORES` and `ZOOM_SCORES` may move (inferred) | the window leaves the strip, so focus, column moves and the overview treat it differently | only applies once Kai folds a window in: a new window always opens as its own column (`specs/strip.md:12-13`) |

H1 would bring in niri's names rini refuses today: `preset-window-heights`
(`src/app/config/kdl/suggest.rs:178`), an absolute `set-window-height "50%"`, and a window rule's
`default-window-height { proportion 0.5; }`.

**Kai's four corners are why H2 fits.** He asks for any of the four corners "wether we have or
don't have other windows on that strip". H1's `align` gives top, middle and bottom: the vertical
half of a corner. The horizontal half has no meaning yet in a strip. With neighbours, a column's
left or right is wherever it sits in the strip, so "top-right" could mean the right edge of the
view, which would have to show empty space beside the column, or the end of the strip (inferred).
H2 has all four corners today because a floating window leaves the strip, with or without
neighbours. What H2 still needs is proportional sizes (S, above): `fixed 748` by `fixed 825` is
1/4 by 1/2 on the 32" only, and the same shape on the 5K2K is 836 by 688.

## Experiments

Undo any of them with `target/release/rini-cli execute config reload`. A `window-rule` in a fragment
replaces the whole running list (`specs/config.md:92-94`), so each fragment carries Kai's 6 running
rules verbatim from `rini-cli execute config get`, followed by the new rules below.

Each fragment was checked with `target/release/rini --validate --config <file>` on scratch copies,
alone and merged into a copy of Kai's `~/.config/rini/config.kdl` the way `config set` overlays it
(`Config::with`, `src/app/config/mod.rs:150`):

| Exp | Result |
|---|---|
| E2 | valid as written below, alone and merged. The one-line form `layout { max-column-width { proportion 1.0; } }` is refused, "unexpected token" at the last `}`: a node closed on one line needs `; }` |
| E3 | valid, alone and merged, without Slack and Outlook |
| E4 | valid, merged into Kai's binds. `Mod+Shift+Equal` and `Mod+Shift+Minus` are unbound in the running config |
| E5 | valid |
| Model B | refused, as expected: `fixed` in `layout › min-column-width`, `near` in every width and height, `output-min-width`, and `proportion` in a rule's width. The block as it stands, with the browser rule, was checked again with a debug build of this branch |

**Cost of every rule trial:** a rule's width beats a launch slot's and a window it sizes neither
takes nor leaves its app's width, so VS Code, Zen, Chrome and Ghostty open at the rule's width
while E2 or E3 runs, and a width Kai gives one of them is lost at its next relaunch. The slot's
workspace is kept. At `eb596883`, before M0, a rule turned the whole slot off (failure 2).

**Baseline:** journal line 507423 of `~/Library/Logs/rini/transitions.jsonl` (2026-11-04). Count
width presses since then with:

```sh
tail -n +507424 ~/Library/Logs/rini/transitions.jsonl \
  | grep -cE '"mark":"press","what":"(cycle_preset_column_width|toggle_fullscreen_within_gaps|resize_window_by)"'
```

| # | Where, how long | Change | What to look for |
|---|---|---|---|
| E1 | Not run | Answered by Kai on 2026-11-04 ("Kai's answers"): the 5K2K runs at 3360x1418, and the two 1920x1200 displays are discarded | — |
| E2 | 16" alone, 2 days | `max-column-width` to 1.0; wide apps `fixed 1722`, Ghostty `fixed 1148` | Width presses on fresh windows. **Cost:** it undoes Kai's 0.66667 cap, set so that `Mod+Equal` and `Mod+Minus` "cannot crawl to 100%" (`~/.config/rini/config.kdl:140-148`). A bounded 1.0 column is not `Mod+F`'s mode; what `Mod+F` does on it is unchecked. Do not dock with it |
| E3 | 32" docked, 3 days | `min-column-width` 0.25, 1/4 added to the presets; VS Code, Kiro, Kiro Crew, Zen and Chrome `fixed 1500`, Ghostty `fixed 999`, Finder and Books `fixed 748` | Is 1/4 too narrow, or only too tall? On day 2, Ghostty to `fixed 1500` to compare a 1/2 terminal, and Finder and Books to `fixed 999` for Kai's 1/3. **Slack and Outlook stay out:** they are pinned to the laptop (`~/.config/rini/config.kdl:284-289`), where `fixed 1500` clamps to 1148 under max 0.66667 and would shrink two windows that are full today. Windows opened on the laptop get clamped values |
| E4 | 32" docked, 1 day | Bind `Mod+Shift+Equal` and `Mod+Shift+Minus` to `set-window-height "+5%"` and `"-5%"` (both unbound) | VS Code at 1/2, Ghostty folded under with `Mod+Comma`, 5 presses to 25%. Does 3/4 + 1/4 beat 1/2 + 1/3 side by side? |
| E5 | 32" docked, 2 days | Finder: `open-floating true`, `default-floating-position x=3 y=3 relative-to="top-right"`, `fixed 748` by `fixed 825` | Is 1/4 by 1/2 the right shape, and which corner? H2 is chosen (Kai's answer 3), so this sets its size, not the path. The same shape on the 5K2K is 836 by 688 |
| E6 | Code (S-M), then 1 week on both setups | `near N` in a window rule's `default-column-width`: snap to the nearest preset after the min and max clamp | B in miniature: one fragment serves the laptop and the 32", which `fixed` cannot |
| E7 | After M0, 1 week | Re-run the count | Width presses a day against about 3 (45 over 16 non-lab days) and 14 on 11-04. A repeat `Mod+F` on a window already full is near 0, and none within 3 minutes of a wake |

E2's new rules (E3 and E5 follow the same shape with the values above):

```kdl
layout {
    max-column-width { proportion 1.0; }
}
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

## Kai's answers

Kai, 2026-11-04, to the three questions this research asked: "1. it runs on 3360x1418. the two
1200p screens you can discard 2. yes 3. yes".

| # | Question | Answer | Decision |
|---|---|---|---|
| 1 | Does the 5K2K run at 3840x1620 looks-like, and what are the two 1920x1200 displays? | 3360x1418; discard the 1920x1200 displays | **The fit is recomputed at 3360x1418:** B reproduces 16 of 20 cells, 18 with two display-width rules. The 1920x1200 displays are out of every table and fit; no slot names one, so the laptop's slots while docked to them still count. The 5K2K's pt/in stays unknown; B does not need it |
| 2 | May an app's width carry across arrangements, below the per-window record (M0.3)? | yes | **M0.3 goes ahead.** `specs/displays.md` is amended in place: a window's records still share nothing, and an app's width MAY carry below them |
| 3 | Is a utility window floating in a corner (H2) acceptable, or must it stay in the strip (H1)? | yes | **H2 is the path for utility windows on big displays; H1 is deferred.** E5 now sets H2's size and corner |

The same change and its review add Kai's report as requirements in `specs/displays.md`: a width
the user set survives a wake, a re-add, a drag and a relaunch under an opening rule that sets no
width or has not changed it; a rule's changed width wins over older memory; and a new window of a
running app opens at the app's last width. "What the specs promise against this" says how far M0
meets each.

## Needs checking

- Whether M0 holds a width across a real sleep and wake (failure 8). Its tests drive candidate
  (a); one sleep on the laptop alone with a window at full width, read against the `info` erase
  and give-up lines, answers it.
- Why no docked slot names the 32". Next time docked, compare each window's `home_display` in
  `rini-cli query diagnostics` with the display it sits on.
- Saved launch-slot offsets are not shifted when `default-column-width` changes live, while live
  columns are (inferred). Do not change it during the trials.
- Only if H1 comes back: what `move-window-up` and `move-window-down` do on a lone window, which
  bears on H1's alignment keys.
- Whether a title change runs a rule's width resize again, which Obsidian's title rule needs.
- The 5K2K's physical size, for the millimetre comparison only (34" or 40" moves leave-one-out
  between 16 and 15 of 20). B does not need it.

## Side quests (parked)

- Stored bounded offsets run past the clamp: Ghostty `Offset(1.36667)` and `Offset(4.76667)` in
  `layout.ron` against a 0.66667 max. They clamp when drawn
  (`src/layout/domain/constraints.rs:63-70`); where they come from is unexplained.
- `resize_selection_by` with nothing selected rewrites the strip's `column_width_ratio`
  (`src/layout/domain/scrolling/width.rs:166-172`).
- Autosave is not held back while displays churn (`src/app/reactor/autosave.rs:53-63`), so a save
  mid-change can file one arrangement's windows under another's key (inferred, low impact).
