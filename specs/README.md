# What rini must do

A spec here says what the software is REQUIRED to do and why. `docs/` says how the code came to be
that way — measurements, findings, rejected approaches. When the two would overlap, the requirement
goes here and the evidence stays there, with a link.

A spec is written so that someone who has never seen the code can tell whether a build satisfies it.
That means no file paths in a requirement, no function names in a requirement, and no "as implemented
in". Those belong in the **Where it lives** section at the foot of each spec, which is allowed to rot
loudly — `tests/architecture.rs` checks every path mentioned anywhere in the repo still resolves.

## The rule

**Anything the user reports goes in here, in the same change that acts on it.** A bug, a quirk, a
"can it also do X", an "that's not what I meant" — each one is a requirement being discovered, and the
test that pins it is not enough on its own. A test says the code does something; it does not say the
behaviour was ASKED for, by whom, or what the alternative was that got rejected. Six months later
nobody can tell a deliberate behaviour from an accident that happens to be tested.

So every requirement that came from a report carries a **Reported** line: what was observed, when, and
what it turned out to be. That line is the thing a future change has to argue with before it alters
the behaviour.

Rules for keeping this honest:

1. A requirement is stated as an obligation — MUST, MUST NOT, or MAY — not as a description of the
   code. "The strip scrolls between column starts" is a description. "A column MUST NOT be wider than
   the viewport, because the strip scrolls between column starts and never pans within one" is a
   requirement with its reason.
2. When behaviour changes, the requirement changes in the SAME commit. A spec that contradicts the
   build is worse than no spec.
3. When a requirement is dropped, delete it and say so in the commit. Do not leave it with a note
   saying it no longer applies.
4. Contradictions get resolved, not stacked. If a new report contradicts an existing requirement, the
   old one is rewritten and its **Reported** line is kept, because how it used to be asked for is part
   of why it changed.
5. Numbers and dates are exact or absent. No "recently", no "a few".

## The specs

| | |
|---|---|
| [`windows.md`](windows.md) | Which windows rini takes on, and which it refuses |
| [`strip.md`](strip.md) | The one scrolling layout: columns, widths, folding, maximising |
| [`workspaces.md`](workspaces.md) | The vertical stack of workspaces, and what belongs to one |
| [`displays.md`](displays.md) | More than one screen, and surviving a replug |
| [`focus.md`](focus.md) | Who has focus, and what is in front of what |
| [`input.md`](input.md) | Bindings, modifiers, and the gestures |
| [`persistence.md`](persistence.md) | What survives a restart, and what a bad file costs |
| [`animation.md`](animation.md) | What moves, and what must never be seen to move |

## What rini is

An opinionated remake of the niri experience for macOS: **one** scrolling layout, one strip of columns
per display, with workspaces stacked vertically. Not a general tiling framework — there is no second
layout mode, and adding one is out of scope rather than unimplemented.
