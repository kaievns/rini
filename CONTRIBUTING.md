# Contributing to rini

These rules hold for every change, whoever writes it, including an AI agent. Most of them are checked by
`bin/check.sh`, and a change that breaks one fails there. The rest are checked in review.

## The one check

`bin/check.sh` MUST pass before every commit. CI runs it and nothing else (`.github/workflows/rust.yml`).

| Step | What it runs |
|---|---|
| fmt | nightly `cargo fmt --all --check` (`rustfmt.toml` needs nightly) |
| build | `cargo build --workspace --all-targets` with `-D warnings` |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings`, lints from `[workspace.lints]` |
| test | `cargo test --workspace`, in parallel, including the checks in `tests/` |

It builds into `target-dev`. Format with:

```sh
RUSTFMT="$(rustup which --toolchain nightly rustfmt)" cargo fmt --all
```

Never build into `target/release`: the service runs that binary (see "Building and deploying").

## The bar

The codebase is held at these eight criteria. Each one has a check, and a change MUST keep every
check green. Read a failure as the rule talking. Never weaken a check to make a change pass.

| # | Criterion | Held by |
|---|---|---|
| A1 | the check is green locally and in CI | `bin/check.sh`, `tests/check.rs` |
| A2 | no non-test function over 100 code lines, no non-test file over 1500 | `no_function_or_file_grows_past_its_limit` |
| A3 | no rule reads the platform directly; tests state answers through fakes, never overrides | `a_domain_never_reads_its_own_adapters`, `the_window_server_is_reached_only_through_its_port`, `no_test_states_an_answer_through_an_override` |
| A4 | the suite runs in parallel in seconds | `bin/check.sh` timing; `docs/testing.md` |
| A5 | every `unsafe` states its invariant; no `static mut` | `[workspace.lints]` (`undocumented_unsafe_blocks`, `unsafe_op_in_unsafe_fn`, `missing_safety_doc` denied), `the_workspace_denies_undocumented_unsafe_and_no_source_allows_it`, `nothing_in_the_repo_holds_a_static_mut` |
| A6 | docs and specs match the code; no history narrated in comments; no hand-written counts | `every_documented_path_resolves`, `every_long_name_a_doc_cites_is_still_defined`, `every_domain_and_reactor_module_is_named_in_its_readme`, `no_comment_in_src_or_crates_narrates_the_code_s_history` |
| A7 | types say what values mean: no 2+ bool parameters, no state machine as loose bools, no positional tuple over 3 | `fn_params_excessive_bools` denied, `no_production_type_holds_a_positional_tuple_over_three`, `no_long_parameter_list_tangled_type_or_bool_run_is_allowed` |
| A8 | no panic reachable from real input on a daemon thread | review; `no_panic_is_caught_outside_a_test` |

`tests/architecture.rs` also holds the layering: features never import the application, `domain/`
never touches macOS, and each feature reaches the platform only through its port.

## Writing code

- **Find the feature first.** `docs/architecture.md` maps every feature. A feature's `domain/` holds
  its rules and is pure. Its `platform/` holds the macOS adapters. A rule takes the facts it needs as
  values, and the adapter reads them.
- **Never put a `#[cfg(test)]` branch inside a rule.** A fake belongs at the boundary, behind the
  port the code already takes. `docs/testing.md`, "Which seam to use", names the seam for each
  platform read.
- **Let the types carry the meaning.** Use an enum for a state machine or a mode, and a struct for
  named fields. A function MUST NOT take two `bool` parameters. A type MUST NOT hold a positional
  tuple of more than three fields.
- **Split code when it outgrows the limits.** The limits are 100 code lines for a function and 1500
  for a file. Split on the code's own seams. There is no exceptions list, and it MUST stay empty.
- **Unsafe.** Each `unsafe` block carries a `// SAFETY:` line naming the invariant that makes it
  sound, for example the Create/Copy ownership rule, nullability, the thread or a lifetime. Each
  `unsafe fn` has a `# Safety` section. When many call sites share one invariant, write one safe
  wrapper. A safe function MUST be sound for every input its signature accepts.
- **Panics.** Code a real event can reach MUST NOT `expect`, `unwrap` or `assert` on what that
  event carries. Warn and skip instead. A panic aborts the daemon (`specs/service.md`).
- **Comments state constraints only:** a measured platform behaviour, or a value that looks
  arbitrary and is not. Never say where code came from, what it used to do, or why a change is
  right. Findings go in `docs/`.
- **Remove the old path in the same change.** That covers dead branches, duplicate helpers and two
  records of one fact. Deleting a behaviour deletes its tests, its docs and its spec in the same
  commit.
- **Allows.** A clippy `#[allow]` sits on the narrowest item and carries a `reason` naming the real
  constraint. A file-wide allow is listed in `docs/testing.md`.

## Tests

- **Cover every change with a test that fails without it.** Check this by undoing the change and
  watching the test fail. A test that passes either way covers nothing.
- **Extract the logic before testing it.** When something looks untestable, pull the decision out
  into a pure function that takes values, and test that. Rules about sizes, order or eligibility
  are pure functions.
- **State answers through the fakes,** such as `FakeWindowServer`, `FakeAx`, `FakeCapturer` and the
  in-memory `SystemReads`. A fake answers "unknown" until a test states otherwise. Reading the live
  window server from a test is a bug.
- **Guard process statics.** A test that writes one holds its guard (`docs/testing.md`, "A process
  static is shared by every test"). A test that reads an animation back off a layer holds
  `hold_core_animation`.
- **Animation looks.** Every look passes the `every_look_*` contracts in
  `src/animation/domain/choreography.rs`. Slide's and Zoom's seeded output is pinned by
  `SLIDE_SCORES` and `ZOOM_SCORES`. Change a pin only when the look is meant to change, and say why
  in the commit. A new contract or tolerance needs a deliberately broken look that it rejects.
- **Name a test after the behaviour it holds,** as a sentence. A reactor test reads the reactor
  through its accessors (`src/app/reactor/testing.rs`), never its fields.
- **Leave screen tests to the battery.** Those that drive the real display run only after 300s with
  no input (`tools/focuslab/README.md`). Unit tests never touch the screen.

## Docs and specs

There are three records, and each fact lives in one of them (`docs/architecture.md`, "Where
documentation lives"):

- **`specs/` is what rini must do.** A reported bug, quirk or request becomes a requirement in the
  same change that acts on it. The requirement is a MUST or MUST NOT with its reason, followed by a
  **Reported** line quoting the report, dated. Behaviour found without a report gets a **Found**
  line. `specs/README.md` has the full rule.
- **`docs/` is how the code came to be that way:** measurements, findings and rejected approaches.
  It is also the home of anything a comment would otherwise narrate.
- **`src/<feature>/docs/README.md` explains each feature:** what it owns, a reading order, and a
  row for every module under `domain/`. Each crate has the same.

Keeping the records current:

- When code a doc describes changes, correct the doc in the same commit. Never append a newer
  section that contradicts an older one.
- Write numbers as measured, with the date or commit, or leave them out. Sizes and counts that the
  checks hold are cited by the check's name, not copied.
- Keep README lines under 80 words, and wrap prose at 100 columns.

## Commits

- One logical change per commit, with the check passing before it.
- The message is one line saying what now holds and which test holds it. `git log --oneline` shows
  the style.
- A merge says what the merged branch now holds: `Merge <branch>: <what holds>`.
- Every change is reviewed before it reaches `main`, by someone other than its author or by an
  adversarial review pass. A review hunts for behaviour changes, tests that would pass without the
  change, weakened checks, and docs that drift.

## Building and deploying

- **Build anything but a deploy into another directory,** such as `target-dev` or a worktree's own
  `CARGO_TARGET_DIR`. `bin/sign-rustc.sh` refuses a compile into `target/release` unless
  `RINI_DEPLOY=1`, which only `bin/build.sh` sets.
- **The service runs exactly one path, `target/release/rini`.** macOS ties Accessibility and Screen
  Recording to that path and the signature (`docs/signing.md`).
- **To deploy,** move a signed build into place with a rename, then restart through that binary:
  `cp <built> target/release/rini.new && mv -f target/release/rini.new target/release/rini`, then
  `target/release/rini service restart`. Never add `--move`.
