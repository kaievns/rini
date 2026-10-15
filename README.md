<div align="center">

# rini

**A scrolling window manager for macOS. One layout, done properly.**

rini is a hard fork of [rift](https://github.com/acsandmann/rift), remade around the scrolling strip
of [niri](https://github.com/YaLTeR/niri). One strip of columns per display. Workspaces stacked
vertically. Everything it needs, built in.

</div>

## Why it exists

I want niri on a Mac. Not a tiling framework with a niri mode, and not niri plus five other apps to
make it look right. Three positions follow from that.

**One layout.** Rift is a good general-purpose window manager with five layout modes. I use one.
Some hard bugs belong to a scrolling layout alone. What does a column's width mean? Where does a
scrolled-away window live? What is "the same workspace" on two displays? A codebase that must also
be right for BSP and main-stack answers them conservatively, or not at all. rini answers them for
scrolling only. A second layout mode is out of scope, not unbuilt (`specs/README.md`).

**Batteries included.** rini must meet every requirement with nothing but itself and macOS, and
carry no code that exists only to accommodate another app's tool (`specs/README.md`, decided
2026-10-06). What it needs drawn, it draws: the menu bar, the focus ring and border, the window
switcher and every animation. One path still breaks the rule, listed in `roadmap.md`.

**A Mac that feels like niri.** niri's config format and names, niri's focus ring and border, and
every layout change drawn as motion. Where rini differs from niri, `docs/config.md` says where and
why.

Nearly every change here alters shared behaviour, deletes a subsystem or reshapes the saved state. A
soft fork tracking upstream could not carry that. So this is a hard fork, with no intention of
merging back.

## What it does

### The strip

- One strip of columns per display. A column holds one or more windows stacked vertically, and the
  strip scrolls between column starts. A column is never wider than the viewport (`specs/strip.md`).
- A new column is 0.7 of the viewport. The resize keys (`set-column-width`) hold a column between
  0.3 and 0.9. `Mod+R`'s presets are a third, a half, two thirds and the whole viewport, stepped in
  order and wrapping. A drag is held by neither (`specs/strip.md`, `rini.default.kdl` `layout`).
- The resize keys and `Mod+R` grow and shrink a column about where it stood when it was focused.
  The preset at 100% is a width: the stack stays a stack.
- Full width (`maximize-column`) keeps the window on the strip, with no macOS fullscreen. A stacked
  window comes out into the stack's place and goes back, scroll position included. A title-bar
  double-click is the same toggle, and full width survives a restart.
- Folding moves the focused window into the column on its left or right. The same key folds it back
  out to the row it came from.
- Either edge drags. A left-edge drag moves the left edge and keeps the right one. On a stacked
  window, a top or bottom drag trades height with the window across that edge. Dragging a window
  over another swaps them.
- Floating windows keep a position per workspace. macOS native fullscreen takes a window out of
  rini's hands, and leaving it puts the window back.

### Workspaces and displays

- Workspaces are a vertical stack. Their order is global; each display shows its own
  (`specs/workspaces.md`). The built-in can be on one while the external is on another.
- Left and right move along the strip, and stop and bounce at its end. They never cross displays,
  and that is not configurable (`specs/focus.md`).
- Up and down cross displays: `Mod+J` and `Mod+K` move within a column, then to the display below
  or above. The workspace stack is reached by number, `Mod+1`..`Mod+4`, or by `focus-workspace-up`
  and `-down`, which are unbound by default.
- The default binds send a window to another workspace without following it (`focus=false`). The
  action itself follows unless told otherwise, as niri's does.
- A display's identity is its UUID. macOS mints a new space id on every reconnect: one monitor was
  479, 484, 487, 516, 552 and 1138 in a single session (`specs/displays.md`). Unplug and replug
  returns your windows in their order.
- Each window remembers its display, its width there and its place in the strip per arrangement:
  the set of displays attached. Docked and undocked are two layouts, and neither overwrites the
  other. Widths are kept as ratios, never points.
- A window's display is never guessed from its coordinates. Parked windows sit at coordinates that
  land on a neighbour display, and trusting them walks every window onto one screen
  (`src/workspaces/docs/README.md`).
- `open-on-output "internal"` or `"external"` pins an app to a display by role, so the rule holds at
  every desk.
- A relaunched app's windows return to their workspace and width, keyed by bundle id. A restart
  restores the layout from `~/.rini/layout.ron`.

### Motion

- Every layout change, scroll, workspace switch, move, window open and resize flies on a Core
  Animation overlay of window pictures. The real windows are placed once, behind it
  (`specs/animation.md`).
- The pictures are taken at the press. A batched SkyLight capture takes every window on screen, the
  windows behind the strip included. ScreenCaptureKit takes a window cut by the display's edge or
  off it. The overlay waits at most 80ms for them.
- A resize clips a window's content against the edge that moves. It never stretches it.
- A push against the end of a strip or the workspace stack bounces the view 72pt, and every push
  shows.
- Animation is cosmetic and never load-bearing: with it off, rini behaves the same. A movement takes
  0.35s times `slowdown`. `animations { off; }` or `--no-animate` turns it off, and Low Power Mode
  skips it.

### What rini draws

- **The focus ring.** niri's, on each workspace's active window: 2pt, `#7fc8ff` active and `#505050`
  inactive, on by default. niri's default is 4pt (`specs/decorations.md`).
- **The border.** niri's, off by default: 4pt, taken out of the window's place in the strip, so the
  layout is the same with it on or off.
- Both take niri's gradients and per-window rules. Urgent paint comes from an app's Dock-bounce
  attention ask. With both off, they cost no work.
- **The bar.** One 32pt bar per display, on a still blurred picture of the desktop. It shows the
  workspace numerals, the shown workspace's apps, the focused app and title, menu extras and a
  24-hour clock (`specs/bar.md`). It replaced my sketchybar setup on 2026-09-29. It shows `✕ config`
  while the config cannot be used. `bar { off; }` turns it off.
- **The switcher.** Three scopes share one switcher, all most recent first. One offers every window,
  a stack per app. One offers the focused workspace on every display. One offers the focused app's
  windows wherever they are (`specs/switcher.md`). A tap steps without a popup. Holding shows a
  popup of window pictures, and releasing commits.

The switcher needs a chord of your choosing. macOS does not give up Cmd-Tab: the Dock's switcher is
a window-server hotkey, and dropping the key in an event tap cannot take back a dispatch already
made. My keyboard layer sends Cmd+F17, Cmd+F18 and Cmd+F19 instead. With the chord redirected, a
wedged rini cannot fall back to macOS's switcher, and Secure Input silences all three.

### What it no longer has

- Rift's traditional, BSP, main-stack and stack layouts, and the layout picker (2026-08-12).
- Rift's menu bar, stack line and overview overlay, and its per-frame Accessibility animation engine
  (2026-09-18).
- The TOML config (2026-10-01), the border-tool companion code (2026-10-08) and the trackpad swipes
  (2026-10-10). rini taps no trackpad gesture (`specs/input.md`).

## Status

**Personal project, used daily on one machine.** It works well for me. I am not looking for users, I
make breaking changes without warning, and config keys change when a better design needs them. The
layout file makes two promises: a remembered width stays readable by an older rini
(`specs/displays.md`), and a file from a newer schema is refused rather than half-read
(`specs/persistence.md`). If you want a stable macOS
tiling window manager with a community behind it, use [rift](https://github.com/acsandmann/rift).

Open work is in `roadmap.md`. What a build does not yet do is the "Not yet met" lines in `specs/`.
What is not yet where the code map says is the last table in `docs/architecture.md`.

## Requirements

- **"Displays have separate Spaces" on.** rini refuses to start without it and says why
  (`specs/displays.md`).
- **Accessibility and Screen Recording**, both keyed to the signed binary at its path. Without
  Screen Recording every capture returns nothing and flights fly empty tiles
  (`docs/permissions-and-the-launch-agent.md`).
- **No SIP change.** rini lays virtual workspaces over one native Space per display.
- macOS 26 is the only version it has run on (Darwin 25.6,
  `docs/permissions-and-the-launch-agent.md`).
- A Rust toolchain, plus nightly for formatting.

## Building and running

```sh
./bin/build.sh                         # cargo build --release, sign target/release/rini, check the signature
target/release/rini --validate         # check the config and the saved layout, then exit
target/release/rini service install    # install the launchd agent
target/release/rini service start      # run it; also stop, restart, uninstall
```

`--validate` needs the Accessibility grant and separate Spaces, like a normal start: on a fresh
build it waits on the Accessibility prompt first.

**Every cargo build is signed.** `.cargo/config.toml` runs rustc through `bin/sign-rustc.sh`, which
signs the `rini` binary with the `rini-dev` certificate as `git.kaievns.rini`. Without the
certificate it skips signing rather than failing. macOS ties the Accessibility grant to the code
identity, and an ad-hoc signature changes with every build, so each rebuild would cost a re-grant.
`bin/build.sh` adds a check that fails loudly when the signature has no certificate in it.
`docs/signing.md` has the certificate recipe.

**The service runs the build that starts it.** `service start` and `service restart` point the
agent at the binary that runs them, symlinks resolved. The grants are tied to the path as well as
the signature, so a binary at a new path is a client macOS never granted. Both refuse a path change
unless given `--move` (`specs/service.md`).

**A release build in the main checkout is a deploy.** launchd runs `target/release/rini` with
KeepAlive, so the next crash or restart picks up whatever is there. Builds for anything else go
elsewhere:

```sh
CARGO_TARGET_DIR=target-dev cargo build --release    # also for cargo test --release and cargo bench
```

To deploy another revision, build and sign it in its own target dir, then move it into place and
restart. The rename gives a new inode; copying over the running binary gets it killed on its next
page-in. `tools/focuslab/measure.sh --deploy <binary>` does the same with checks before and after.

```sh
cp <built> target/release/rini.new && mv -f target/release/rini.new target/release/rini
target/release/rini service restart
```

**rini starts managing nothing.** On every start, each native Space is inactive until `Mod+Z`
(`toggle-space-activated`) turns it on. Write `activate-spaces-by-default` to manage every Space
from the start.

Other flags: `--one` (this Space only), `--no-restore` (start from a clean layout), `--config PATH`,
`--default-disable`. The agent logs to `/tmp/rini_<user>.out.log` and
`/tmp/rini_<user>.err.log`.

`cargo test` runs on one thread on purpose: concurrent framework start-up aborts the process
(`docs/testing.md`). Formatting needs nightly, because `rustfmt.toml` enables unstable options;
stable `cargo fmt` ignores them and reflows every file.

```sh
rustup toolchain install nightly
cargo +nightly fmt
```

## Configuration

The config is `~/.config/rini/config.kdl`, in niri's format: KDL v1, with niri's section names, node
names, nesting and values wherever the two share a concept. niri's
[configuration wiki](https://niri-wm.github.io/niri/Configuration:-Introduction) explains the shared
sections. A niri name rini lacks is refused with the reason. Where rini differs, and why, is
`docs/config.md`; the requirements are `specs/config.md`.

`rini.default.kdl` is the annotated reference. It is compiled in and used when the file is missing
or cannot be used. The file reloads on every change. Every problem goes into one report, with its
line and column, bar the two kinds `specs/config.md` lets wait for another fix. A file that cannot
be used at launch means the built-in defaults; a reload that cannot be used keeps the running
config. `rini-cli execute config set '<KDL>'` changes it at runtime.

The default binds, with `Mod` as Alt (`input › mod-key`):

| keys | action |
|---|---|
| `Mod+H` / `Mod+L` | focus the column left or right |
| `Mod+J` / `Mod+K` | focus the window below or above, then the display below or above |
| `Mod+Shift+H/J/K/L` | move the window |
| `Mod+1`..`Mod+4` | show a workspace |
| `Mod+Shift+1`..`Mod+Shift+4` | send the window to a workspace, focus staying here |
| `Mod+Tab` | the previous workspace |
| `Mod+Comma` / `Mod+Period` | fold into the column left or right, and back |
| `Mod+Shift+Comma` | split a joined column into columns, or join the column on the left |
| `Mod+F` | full width |
| `Mod+Shift+Space` | float or tile |
| `Mod+Shift+V` | focus the floating windows, and back |
| `Mod+Shift+Equal` / `Mod+Shift+Minus` | width 5% up or down |
| `Mod+Z` | manage this Space, or stop |
| `Mod+Return` | open Terminal |
| `Mod+Ctrl+Q` | save the layout and quit |

Not bound by default: `switch-preset-column-width` (the `Mod+R` the specs name),
`focus-workspace-up` and `-down`, and the switcher, which goes in `recent-windows › binds`.

## How the project is kept

- **`specs/` is what rini must do.** Requirements as obligations; each one that came from a report
  carries a **Reported** line quoting it. Anything I report becomes a requirement in the same change
  that acts on it (`specs/README.md`).
- **`docs/` is how the code came to be that way.** Measurements, findings and rejected approaches.
  Each feature and each library has its own, `src/<feature>/docs/README.md` and
  `crates/<crate>/docs/README.md`; `docs/` holds only what spans features.
  `docs/architecture.md` is the map.
- **`tests/architecture.rs` holds the shape.** Every feature's `domain/` stays free of macOS, every
  path the docs mention still resolves, and every spec is listed in the index.
- **What a flight costs is measured on the screen.** The timing battery in `tools/focuslab/` flies a
  fixed set of pans and switches and judges a build on 87 metrics. Screen recordings show how a
  resize looks. Both run only after 300s with no input, and stop at the first foreign event
  (`tools/focuslab/README.md`).
- **Every transition can be read back.** The journal keeps the last 8192 steps in memory. With
  `animations { journal }` it writes them to `~/Library/Logs/rini/transitions.jsonl`
  (`src/transitions/docs/journal.md`).

## Interop

Mach-port IPC, as in rift, under `git.kaievns.rini`. `rini-cli` prints compact JSON;
`RINI_CLI_PRETTY=1` indents it.

```sh
rini-cli query diagnostics             # every display, workspace, column and parked window
rini-cli query displays
rini-cli execute workspace switch 1    # counts from 0: the second workspace
```

The CLI counts workspaces from 0, the config from 1. rini pushes no events: a tool that follows
rini polls a query.

## Credits

rini is a hard fork of [rift](https://github.com/acsandmann/rift) by
[acsandmann](https://github.com/acsandmann), itself a fork of
[glide-wm](https://github.com/glide-wm/glide) by tmandry. Rift built the foundations rini stands on:
the actor architecture, the private-API work and the IPC layer. This fork would not exist without
it. The animation is rini's own: rift's per-frame Accessibility engine went on 2026-09-18, and the
capture overlay that replaced it began on 2026-08-16. Copyright and licence are unchanged; see
[LICENSE](LICENSE).

It uses private APIs reverse engineered by yabai and other projects. Not affiliated with rift,
glide-wm or yabai.
