# Working on rini as an agent

Follow `CONTRIBUTING.md`. Every rule there applies to you. This file adds how an agent works so
that a change reaches `main` at that bar.

## Before a change

- Read the feature's `src/<feature>/docs/README.md` and the spec in `specs/` that covers the
  behaviour.
- A behaviour the user reports or asks for becomes a requirement in `specs/` in the same change,
  with a **Reported** line giving the report and its date (`specs/README.md`).

## While changing

- **Work in your own worktree.** Create it with
  `git worktree add /tmp/rini-<task> -b <task>/<stream> main`. Give it its own
  `CARGO_TARGET_DIR=/tmp/rini-<task>-target`. Parallel streams each get their own pair. Never
  edit the main checkout while streams run.
- **Run `bin/check.sh` before every commit,** and report its real counts. A build that succeeds is
  not a verified change.
- **Undo the change once and watch the new test fail.** Say in the report that you did.
- **Never raise a check's limit, and never add an exception or an `#[allow]` to get past a check.**
  Fix the cause. If a check is wrong, change it in its own commit, with the reason in `specs/` or
  `docs/testing.md`.

## Before merging

- **Review every step adversarially,** by a separate reviewer that hunts for:
  - behaviour changes hidden in a refactor;
  - tests that would pass without the change;
  - weakened checks;
  - `SLIDE_SCORES` or `ZOOM_SCORES` moving without a reason;
  - docs that drift.
- **Fix every high and medium finding,** and argue in writing where you disagree.
- **Merge into an integration branch,** run `bin/check.sh` there twice, then fast-forward `main`.
  Remove the worktrees and their target directories afterwards.

## Never, without the user's say-so

- **Never build into the main checkout's `target/release`,** and never run `rini service` from any
  other binary. The service path rules are in `CONTRIBUTING.md`, "Building and deploying".
- **Never drive the screen but through the battery or a recording,** under the idle rule in
  `tools/focuslab/README.md`.
- **Never edit the user's config in `~/.config/rini`.** To try a setting live, use
  `rini-cli execute config set '<kdl>'`. It lasts until the next reload (`specs/config.md`).
- **Never push.**
