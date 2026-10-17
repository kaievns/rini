# Contributing to rini

These rules hold for every change, whoever writes it, including an AI agent. Most of them are
checked by `bin/check.sh`, and a change that breaks one fails there. The rest are checked in review.

## The one check

`bin/check.sh` MUST pass before every commit. CI runs it and no cargo command of its own
(`ci_runs_the_check_and_no_cargo_command_of_its_own`). What it runs, where it builds and how to
format are in `README.md`, "Building and running", and in `bin/check.sh` itself.

It agrees with CI only on CI's toolchains: the pinned Rust and a nightly with rustfmt, both named
under `README.md`, "Requirements". On another stable, clippy can pass here and fail in CI, or the
reverse (`docs/testing.md`, "Clippy is a check, at zero warnings").

Never build into the main checkout's `target/release`: the service runs that binary (see "Building
and deploying").

## The bar

The codebase is held at these eight criteria. Each row names what holds it. A change MUST keep
every check green, and review holds what no check does. Read a failure as the rule talking. Never
weaken a check to make a change pass.

| # | Criterion | Held by |
|---|---|---|
| A1 | `bin/check.sh` is green in CI, and locally on CI's toolchains | `tests/check.rs`: `the_check_formats_builds_lints_and_tests_into_target_dev_with_warnings_as_errors`, `ci_runs_the_check_and_no_cargo_command_of_its_own` |
| A2 | no non-test function over 100 code lines, no non-test file over 1500, comments and blank lines not counted | `no_function_or_file_grows_past_its_limit`, its `LONG_FNS` and `LONG_FILES` empty |
| A3 | a feature touches macOS only in its `platform/`, but for the one file in `MIXED`; tests state answers through fakes, never overrides | `nothing_outside_platform_touches_macos`, with review keeping `MIXED` at one file, `a_domain_never_reads_its_own_adapters`, `the_window_server_is_reached_only_through_its_port`, `no_test_states_an_answer_through_an_override`, `a_test_build_never_takes_the_live_window_server` |
| A4 | the suite runs in parallel in seconds | in parallel: `nothing_sets_the_number_of_threads_the_suite_runs_on`; in seconds: no check; measured in `docs/testing.md`, "The suite runs in parallel" |
| A5 | every `unsafe` states its invariant; no `static mut` | `[workspace.lints]` (`undocumented_unsafe_blocks`, `unsafe_op_in_unsafe_fn`, `missing_safety_doc` denied), `the_workspace_denies_undocumented_unsafe_and_no_source_allows_it`, `nothing_in_the_repo_holds_a_static_mut` |
| A6 | docs and specs match the code; no history narrated in comments; a count is measured and dated or left out | `every_documented_path_resolves`, `every_long_name_a_doc_cites_is_still_defined`, `every_name_and_root_file_the_rules_cite_still_exists`, `every_feature_and_crate_documents_itself`, `every_domain_and_reactor_module_is_named_in_its_readme`, `every_spec_is_listed_in_the_index`, `no_comment_in_src_or_crates_narrates_the_code_s_history`; a lowercase name under four words by review; counts by `the_implementation_audit_states_a_test_count_only_as_measured_then` in that doc and by review elsewhere |
| A7 | types say what values mean: no function with two `bool` parameters, no struct of four `bool` fields, no positional tuple over 3, no long parameter list or tangled type allowed | `fn_params_excessive_bools` (denied, `max-fn-params-bools = 1`), `struct_excessive_bools` (fatal under `-D warnings`), `no_bool_is_taken_by_reference`, `no_production_type_holds_a_positional_tuple_over_three`, `no_long_parameter_list_tangled_type_or_bool_run_is_allowed` |
| A8 | no panic reachable from real input on a daemon thread | review; `no_panic_is_caught_outside_a_test` keeps every panic an abort (`specs/service.md`, "A panic") |

`tests/architecture.rs` also holds the layering (`docs/architecture.md`). A feature never names the
application (`a_feature_never_names_the_application`). No feature file outside its `platform/`
touches macOS, except the one file in `MIXED`, `src/displays/screen.rs`. The application layer,
`src/app/` and `src/main.rs`, wires the features to macOS and is outside that rule.

## Writing code

- **Find the feature first.** `docs/architecture.md` maps every feature. A feature's `domain/`
  holds its rules and is pure. Its `platform/` holds the macOS adapters. A rule takes the facts it
  needs as values, and the adapter reads them.
- **Never put a `#[cfg(test)]` branch inside a rule.** A fake belongs at the boundary, behind the
  port the code already takes. `docs/testing.md`, "Which seam to use", names the seam for each
  platform read.
- **Let the types carry the meaning.** Use an enum for a state machine or a mode, and a struct for
  named fields. A function MUST NOT take two `bool` parameters. A type MUST NOT hold a positional
  tuple of more than three fields.
- **Split code when it outgrows the limits.** The limits are 100 code lines for a function and 1500
  for a file. Split on the code's own seams.
- **The exception lists MUST NOT grow.** In `tests/architecture.rs`, `LONG_FNS` and `LONG_FILES`
  are empty, and `MIXED` holds one file. Review holds all three: fix the code rather than add to a
  list.
- **Unsafe.** Each `unsafe` block and `unsafe impl` carries a `// SAFETY:` comment naming the
  invariant that makes it sound, for example the Create/Copy ownership rule, nullability, the thread
  or a lifetime. Each `unsafe fn` has a `# Safety` section. When many call sites share one
  invariant, write one safe wrapper. A safe function MUST be sound for every input its signature
  accepts.
- **Panics.** Code a real event can reach MUST NOT `expect`, `unwrap` or `assert` on what that
  event carries. Warn and skip instead. A panic aborts the daemon (`specs/service.md`, "A panic").
- **Comments state constraints only:** a measured platform behaviour, or a value that looks
  arbitrary and is not. Never say where code came from, what it used to do, or why a change is
  right. Findings go in `docs/`.
- **Remove the old path in the same change.** That covers dead branches, duplicate helpers and two
  records of one fact. Deleting a behaviour deletes its tests, its docs and its spec in the same
  commit.
- **Allows.** A clippy `#[allow]` sits on the narrowest item and carries a `reason` naming the real
  constraint (`allow_attributes_without_reason`). A file-wide `#![allow]` has its row in
  `docs/testing.md` (`every_file_wide_allow_is_named_in_docs_testing`).

## Tests

- **Cover every change with a test that fails without it.** Check this by undoing the change and
  watching the test fail. A test that passes either way covers nothing.
- **Extract the logic before testing it.** When something looks untestable, pull the decision out
  into a pure function that takes values, and test that. Rules about sizes, order or eligibility
  are pure functions.
- **State answers through the fakes,** such as `FakeWindowServer`, `FakeAx`, `FakeCapturer` and
  `SystemReads::fake`. `FakeWindowServer` and `SystemReads::fake` answer unknown until a test states
  an answer (`every_read_answers_unknown_with_no_setup`,
  `a_test_reactor_reads_unknown_from_everything_it_was_not_told`). A test build cannot take the live
  window server (`a_test_build_never_takes_the_live_window_server`).
- **A static whose value a test reads is injected at its reader,** and each test hands its own
  answer. The table in `docs/testing.md`, "A process static is shared by every test", says what
  each static needs. A test that reads an animation back off a layer holds `hold_core_animation`,
  and every read-back helper asserts the hold (`assert_core_animation_held`).
- **Animation looks.** Every look passes the `every_look_*` contracts in
  `src/animation/domain/choreography.rs`. Slide's and Zoom's seeded output is pinned by
  `SLIDE_SCORES` and `ZOOM_SCORES`. Change a pin only when the look is meant to change, and say why
  in the commit. A new contract or tolerance needs a deliberately broken look that it rejects: a
  `Flawed` look with a `Flaw` of its own. The two thousand-seed sweeps are `#[ignore]`d. Run them
  when a look or a contract changes, as their doc comments say.
- **Name a test after the behaviour it holds,** as a sentence. A reactor test reads the reactor
  through its accessors (`src/app/reactor/testing.rs`), never its fields
  (`a_reactor_test_reaches_no_field_of_the_reactor`).
- **Leave the screen to the battery and the recordings.** They run under the idle rule in
  `tools/focuslab/README.md`. Unit tests never touch the screen.

## Docs and specs

There are three records, and each fact lives in one of them (`docs/architecture.md`, "Where
documentation lives"):

- **`specs/` is what rini must do.** A reported bug, quirk or request becomes a requirement in the
  same change that acts on it. The requirement is a MUST, MUST NOT or MAY with its reason. One that
  came from a report carries a **Reported** line, and one found without a report a **Found** line
  (`specs/README.md`, "The rule").
- **`docs/` is how the code came to be that way:** measurements, findings and rejected approaches.
  It is also the home of anything a comment would otherwise narrate.
- **`src/<feature>/docs/README.md` explains each feature:** what it owns, a reading order, and a
  line naming every module under `domain/`. `src/app/docs/README.md` names every module of
  `reactor/` and `reactor/events/`. Each crate has its own `crates/<crate>/docs/README.md`.

Keeping the records current:

- When code a doc describes changes, correct the doc in the same commit. Never append a newer
  section that contradicts an older one.
- Write numbers as measured, with the date or commit, or leave them out. Sizes and counts that the
  checks hold are cited by the check's name, not copied.
- No README line runs past 80 words (`no_readme_line_runs_past_80_words`). Wrap prose at 100
  columns, which review holds.

## Commits

- One logical change per commit, with the check passing before it.
- The message is one line saying what now holds, which test holds it, and the check's real test
  counts. `git log --oneline` shows the style.
- A merge says what the merged branch now holds: `Merge <branch>: <what holds>`.
- Every change is reviewed before it reaches `main`, by someone other than its author or by an
  adversarial review pass. A review hunts for behaviour changes, tests that would pass without the
  change, weakened checks, and docs that drift.

## Building and deploying

- **Build anything but a deploy into another directory,** such as `target-dev` or a worktree's own
  `CARGO_TARGET_DIR`. A release build in the main checkout is a deploy (`tests/deploy_path.rs`).
- **The service runs exactly one path, `target/release/rini`.** macOS ties the grants to that path
  as well as the signature, so a binary at any other path costs a re-grant (`docs/signing.md`,
  `docs/permissions-and-the-launch-agent.md`).
- **To deploy,** follow `README.md`, "A release build in the main checkout is a deploy". It says
  how the checkout refuses a stray build and how a signed build is moved into place.
- **Never add `--move` unless Kai has decided to move the service.** A move costs a re-grant
  (`specs/service.md`).
