# Values

Rini is a hard fork of [rift](https://github.com/acsandmann/rift). This file replaces
rift's manifesto rather than editing it: several of its values are things this fork
deliberately gives up, and leaving them in place with the name swapped would misrepresent
both projects.

## Values

**One layout, done properly.** Scrollable columns, in the style of
[niri](https://github.com/YaLTeR/niri). Not one layout among several, but the only one. A
scrolling layout raises questions no other layout has to answer: what a column's width
means, what happens to a scrolled-away window, what "the same workspace" means across two
displays. Answering them well requires being free to answer them *only* for scrolling.

**Correctness on multiple displays, ahead of features.** It still comes first, though the
animation has since taken more of the work. A window's display must be durable rather than
inferred, its size must survive a workspace change, and switching one display must not
disturb another. These are not polish; when they are wrong the window manager actively
loses your work.

**Decisions over knobs.** Where a setting exists to avoid choosing, prefer choosing. Each
remaining option should be load-bearing: something with a real reason to differ between
users, not a hedge.

**Batteries included.** rini must meet every requirement with nothing but itself and
macOS. What it needs drawn, it draws, and it carries no code that exists only to
accommodate another app's tool (`specs/README.md`, decided 2026-10-06).

**Diagnosability.** State that cannot be inspected cannot be reasoned about. Every
subsystem holding non-obvious state should be dumpable, hence
`rini-cli query diagnostics`. This value was learned the hard way: several confident
diagnoses in this project turned out to be artefacts of a query that only reported part of
the picture.

**Explain the mechanism, beside the code.** What the software must do goes in `specs/`,
with the report that asked for it; why a fix works, the failing behaviour and the measured
numbers go in `docs/` beside the feature, and the code points at them by path. A fix
whose reasoning is lost gets reverted by the next person to find the code surprising.

**Performance, via private APIs.** Inherited from rift and unchanged. The private APIs
underpin the accessibility API, so they tend to be both more stable and faster, provided
they are used correctly. The AX API is used where unavoidable and treated as the slow path.

## Non-values

**Stability of configuration or persisted state.** Rift aims to avoid breaking changes
after 1.0. This fork explicitly does not: it changes config keys, removes settings, and
reshapes the saved layout file whenever that produces a better design. It keeps only what
`specs/displays.md` and `specs/persistence.md` require: a remembered width an older rini can
read, and a file from a newer schema refused. There is one user.
When that stops being true, this can change.

**Feature breadth.** Multiple layout modes, a layout picker, and settings that exist to
cover every preference are things this fork removes rather than maintains. Breadth is what
a general-purpose window manager owes its users; rift already does that job well, and
anyone wanting it should use rift.

**Merging back upstream.** Nearly every change here alters shared behaviour, deletes a
subsystem, or changes persisted state. None of it is upstreamable, and pretending
otherwise would constrain both projects. Fixes that *are* genuinely general are better
reported upstream as issues than smuggled through a fork.

**Native macOS spaces as the workspace primitive.** Impossible without disabling SIP,
which is not on the table. Rini uses virtual workspaces layered over one native space per
display.

**Avoiding unsafe code.** Unavoidable in something this close to the OS. The goal is to
keep the unsafe surface small and isolated, not to pretend it is absent.
