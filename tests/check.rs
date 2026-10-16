//! `bin/check.sh`, the one command local runs and CI runs, driven against stub `cargo` and `rustup`
//! that record what they were asked: the steps, where they build and with which flags.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const NIGHTLY_RUSTFMT: &str = "/nightly/bin/rustfmt";

/// A directory of stubs that log `CARGO_TARGET_DIR|RUSTFLAGS|RUSTFMT|<tool> <args>` a line per call,
/// and fail any call whose arguments contain `$FAIL`. `rustup which` answers `NIGHTLY_RUSTFMT`.
fn stubs(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rini-check-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for tool in ["cargo", "rustup"] {
        let path = dir.join(tool);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"$CARGO_TARGET_DIR|$RUSTFLAGS|$RUSTFMT|{tool} $*\" >> \"$CHECK_LOG\"\n\
                 case \"$*\" in *\"$FAIL\"*) [ -n \"$FAIL\" ] && exit 1;; esac\n\
                 [ \"{tool} $1\" = \"rustup which\" ] && echo {NIGHTLY_RUSTFMT}\nexit 0\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

fn check(stubs: &Path, env: &[(&str, &str)]) -> (Output, Vec<String>) {
    let log = stubs.join("log");
    let mut command = Command::new(Path::new(ROOT).join("bin/check.sh"));
    command
        .env("PATH", format!("{}:/usr/bin:/bin", stubs.display()))
        .env("CHECK_LOG", &log)
        .env("FAIL", "")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTFMT");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    let calls = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    let _ = fs::remove_dir_all(stubs);
    (output, calls)
}

#[test]
fn the_check_formats_builds_lints_and_tests_into_target_dev_with_warnings_as_errors() {
    let (output, calls) = check(&stubs("pass"), &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = format!("{}/target-dev", fs::canonicalize(ROOT).unwrap().display());
    assert_eq!(calls, [
        format!("{target}|-D warnings||rustup which --toolchain nightly rustfmt"),
        format!(
            "{target}|-D warnings|{NIGHTLY_RUSTFMT}|rustup run nightly cargo fmt --all --check"
        ),
        format!("{target}|-D warnings||cargo build --locked --workspace --all-targets"),
        format!(
            "{target}|-D warnings||cargo clippy --locked --workspace --all-targets -- -D warnings"
        ),
        format!("{target}|-D warnings||cargo test --locked --workspace"),
    ]);
}

#[test]
fn a_failing_step_fails_the_check_and_every_other_step_still_runs() {
    let (output, calls) = check(&stubs("fail"), &[
        ("FAIL", "cargo fmt"),
        ("CARGO_TARGET_DIR", "/elsewhere"),
        ("RUSTFLAGS", "-C debuginfo=0"),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed: fmt"));
    assert_eq!(calls.len(), 5, "{calls:?}");
    assert!(
        calls
            .iter()
            .all(|call| call.starts_with("/elsewhere|-C debuginfo=0 -D warnings|")),
        "{calls:?}"
    );
}

#[test]
fn without_a_nightly_rustfmt_the_format_step_fails_and_runs_no_cargo_fmt() {
    let (output, calls) = check(&stubs("no-nightly"), &[("FAIL", "which")]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed: fmt"));
    assert!(!calls.iter().any(|call| call.contains("cargo fmt")), "{calls:?}");
    assert_eq!(calls.len(), 4, "{calls:?}");
}

/// CI runs the same command as local, and no runner of its own beside it.
#[test]
fn ci_runs_the_check_and_no_cargo_command_of_its_own() {
    let workflow = fs::read_to_string(Path::new(ROOT).join(".github/workflows/rust.yml")).unwrap();
    let runs: Vec<&str> = workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("run:"))
        .map(str::trim)
        .collect();
    assert_eq!(runs, ["bin/check.sh"]);
    assert!(!workflow.contains("nextest"));
}

/// The Rust CI checks and releases with. A new stable can add a lint that fails the check with no
/// change to the code, so CI moves to one only when this and the workflows move with it.
const PINNED_RUST: &str = "1.98.1";

fn workflow(name: &str) -> String {
    fs::read_to_string(Path::new(ROOT).join(".github/workflows").join(name)).unwrap()
}

/// The pinned toolchain CI installs carries clippy, which its minimal profile leaves out.
#[test]
fn ci_installs_clippy_with_the_pinned_toolchain() {
    let workflow = workflow("rust.yml");
    assert!(
        workflow.contains(&format!(
            "uses: dtolnay/rust-toolchain@master\n        with:\n          toolchain: {PINNED_RUST}\n          components: clippy\n"
        )),
        "{workflow}"
    );
}

/// Every workflow that installs a Rust other than nightly's rustfmt installs the pinned one.
#[test]
fn every_workflow_installs_the_pinned_rust() {
    for name in ["rust.yml", "release.yml"] {
        let workflow = workflow(name);
        assert!(!workflow.contains("rust-toolchain@stable"), "{name}");
        let installs: Vec<&str> =
            workflow.split("uses: dtolnay/rust-toolchain@master").skip(1).collect();
        assert!(!installs.is_empty(), "{name} installs no pinned Rust");
        for install in installs {
            let toolchain = install
                .lines()
                .find_map(|line| line.trim().strip_prefix("toolchain: "))
                .unwrap_or_default();
            assert_eq!(toolchain, PINNED_RUST, "{name}");
        }
    }
}

/// Every crate in the workspace takes its lints from `[workspace.lints]`, so a lint set there
/// reaches all of them.
#[test]
fn every_crate_inherits_the_workspace_lints() {
    let manifest = fs::read_to_string(Path::new(ROOT).join("Cargo.toml")).unwrap();
    assert!(manifest.contains("\n[workspace.lints.rust]\n"));
    assert!(manifest.contains("\n[workspace.lints.clippy]\n"));
    let members = manifest
        .lines()
        .find_map(|line| line.strip_prefix("members = ["))
        .unwrap()
        .trim_end_matches(']')
        .split(',')
        .map(|member| member.trim().trim_matches('"').to_owned());
    let crates: Vec<String> = std::iter::once(".".to_owned()).chain(members).collect();
    assert!(crates.len() > 1, "{crates:?}");
    for member in crates {
        let path = Path::new(ROOT).join(&member).join("Cargo.toml");
        let manifest = fs::read_to_string(&path).unwrap();
        assert!(
            manifest.contains("\n[lints]\nworkspace = true\n"),
            "{} does not inherit [workspace.lints]",
            path.display()
        );
    }
}

/// Every Rust source under `dirs`, by path from the root, in path order.
fn rust_sources(dirs: &[&str]) -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    for dir in dirs {
        walk(&Path::new(ROOT).join(dir), &mut files);
    }
    let mut sources: Vec<(String, String)> = files
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(ROOT).unwrap().to_string_lossy().into_owned();
            (relative, fs::read_to_string(&path).unwrap())
        })
        .collect();
    sources.sort();
    sources
}

/// docs/testing.md's section on clippy, where both tables are.
fn clippy_section() -> String {
    let doc = fs::read_to_string(Path::new(ROOT).join("docs/testing.md")).unwrap();
    let (_, section) = doc.split_once("\n## Clippy is a check").unwrap();
    section.split("\n## ").next().unwrap().to_owned()
}

/// The roadmap ids the allows in `source` cite as their reason.
fn roadmap_ids_cited(source: &str) -> impl Iterator<Item = String> + '_ {
    source
        .split("reason = \"P")
        .skip(1)
        .map(|rest| format!("P{}", rest.split('"').next().unwrap()))
}

/// The ids of the rows of the id table in `section`.
fn roadmap_id_rows(section: &str) -> impl Iterator<Item = String> + '_ {
    section
        .lines()
        .filter_map(|line| line.strip_prefix("| P"))
        .map(|rest| format!("P{}", rest.split(' ').next().unwrap()))
}

/// The roadmap is not in the repo, so the row of every id an allow cites is in docs/testing.md, and
/// a row no allow cites is gone from it, down to a table with no rows.
#[test]
fn every_roadmap_id_an_allow_cites_has_a_row_in_docs_testing() {
    let id = format!("P{}.{}", 2, 2);
    assert_eq!(
        roadmap_ids_cited(&format!("#[allow(x, reason = \"{id}\")]")).collect::<Vec<_>>(),
        std::slice::from_ref(&id)
    );
    assert_eq!(
        roadmap_id_rows(&format!("| Id | Task | Done when |\n| {id} | a task | done |"))
            .collect::<Vec<_>>(),
        [id]
    );
    let mut cited: Vec<String> = rust_sources(&["src", "crates"])
        .iter()
        .flat_map(|(_, source)| roadmap_ids_cited(source).collect::<Vec<_>>())
        .collect();
    cited.sort();
    cited.dedup();
    let section = clippy_section();
    assert!(section.contains("\n| Id | Task | Done when |\n"));
    let mut rows: Vec<String> = roadmap_id_rows(&section).collect();
    rows.sort();
    assert_eq!(
        rows, cited,
        "the id table in docs/testing.md against the ids allows cite"
    );
}

/// Whether `line` names a roadmap task by its id, a `P` that starts a word, a number, a dot and a
/// number.
fn cites_roadmap_id(line: &str) -> bool {
    let bytes = line.as_bytes();
    (0..bytes.len()).any(|at| {
        bytes[at] == b'P'
            && (at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_'))
            && bytes.get(at + 1).is_some_and(u8::is_ascii_digit)
            && bytes.get(at + 2) == Some(&b'.')
            && bytes.get(at + 3).is_some_and(u8::is_ascii_digit)
    })
}

/// The roadmap is not in the repo, so its ids are cited only where docs/testing.md copies their
/// rows: in an allow's reason and in docs/testing.md itself. Anywhere else the work is named in
/// words.
#[test]
fn a_roadmap_id_is_cited_only_in_an_allow_s_reason_or_docs_testing() {
    let id = format!("P{}.{}", 2, 2);
    assert!(cites_roadmap_id(&format!("the roadmap's {id}-P2")));
    assert!(!cites_roadmap_id(&format!("S{id}")) && !cites_roadmap_id("P2 and .4"));
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for path in entries.map(|entry| entry.unwrap().path()) {
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let root = Path::new(ROOT);
    let mut files: Vec<PathBuf> = ["README.md", "roadmap.md", "manifesto.md"]
        .iter()
        .map(|name| root.join(name))
        .collect();
    for dir in [
        "src", "crates", "docs", "specs", "tests", "bin", "tools", ".github",
    ] {
        walk(&root.join(dir), &mut files);
    }
    let mut citing = Vec::new();
    for path in files {
        let relative = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if relative == "docs/testing.md" {
            continue;
        }
        for (at, line) in text.lines().enumerate() {
            if cites_roadmap_id(&line.replace("reason = \"P", "reason = \"")) {
                citing.push(format!("{relative}:{}", at + 1));
            }
        }
    }
    assert!(citing.is_empty(), "name the work in words: {citing:#?}");
}

/// Every `#![allow]` over a whole crate or file has its row in docs/testing.md, and every row has
/// its allow.
#[test]
fn every_file_wide_allow_is_named_in_docs_testing() {
    let allowing: Vec<String> = rust_sources(&["src", "crates"])
        .into_iter()
        .filter(|(_, source)| source.lines().any(|line| line.trim_start().starts_with("#![allow(")))
        .map(|(path, _)| path)
        .collect();
    let mut rows: Vec<String> = clippy_section()
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once("` |").map(|(path, _)| path.to_owned()))
        .filter(|path| path.ends_with(".rs"))
        .collect();
    rows.sort();
    assert!(!allowing.is_empty());
    assert_eq!(
        rows, allowing,
        "the file table in docs/testing.md against the files that allow"
    );
}

/// A long parameter list, a tangled type or a run of `bool` parameters is named as a struct or an
/// enum rather than allowed.
#[test]
fn no_long_parameter_list_tangled_type_or_bool_run_is_allowed() {
    let lints = [
        "too_many_arguments",
        "type_complexity",
        "fn_params_excessive_bools",
    ];
    let allowed: Vec<String> = rust_sources(&["src", "crates", "tests"])
        .iter()
        .flat_map(|(path, source)| {
            lints
                .iter()
                .filter(|lint| source.contains(&format!("clippy::{lint}")))
                .map(move |lint| format!("{path}: {lint}"))
        })
        .collect();
    assert!(allowed.is_empty(), "name the parameters instead: {allowed:#?}");
}

/// Whether `line` holds a `bool` behind a reference, `&`, `&mut` or `&'a` before the word `bool`.
fn references_a_bool(line: &str) -> bool {
    line.match_indices('&').any(|(at, _)| {
        let mut rest = &line[at + 1..];
        if let Some(lifetime) = rest.strip_prefix('\'') {
            rest = lifetime.split_once(' ').map_or("", |(_, after)| after);
        }
        rest = rest.strip_prefix("mut ").unwrap_or(rest);
        rest.strip_prefix("bool")
            .is_some_and(|after| !after.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
    })
}

/// Clippy's `fn_params_excessive_bools` counts only a `bool` taken by value, so none is taken by
/// reference: a `bool` is `Copy`, and one written back through `&mut` is a return value.
#[test]
fn no_bool_is_taken_by_reference() {
    let b = "bool";
    for line in [
        format!("fn f(a: &{b}, b: &{b})"),
        format!("fn f(a: &mut {b})"),
        format!("fn f<'a>(a: &'a {b})"),
        format!("let f = |a: &&{b}| **a;"),
    ] {
        assert!(references_a_bool(&line), "{line}");
    }
    for line in [
        format!("fn f(a: {b}, b: {b}s)"),
        format!("g(&{b}s[0], &mut {b}_flag)"),
        format!("x & {b}"),
    ] {
        assert!(!references_a_bool(&line), "{line}");
    }
    let referencing: Vec<String> = rust_sources(&["src", "crates", "tests"])
        .iter()
        .flat_map(|(path, source)| {
            source
                .lines()
                .enumerate()
                .filter(|(_, line)| references_a_bool(line))
                .map(move |(at, _)| format!("{path}:{}", at + 1))
        })
        .collect();
    assert!(
        referencing.is_empty(),
        "take the bool by value: {referencing:#?}"
    );
}
