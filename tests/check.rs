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

/// The stable toolchain CI installs carries clippy, which its minimal profile leaves out.
#[test]
fn ci_installs_clippy_with_the_stable_toolchain() {
    let workflow = fs::read_to_string(Path::new(ROOT).join(".github/workflows/rust.yml")).unwrap();
    assert!(
        workflow.contains(
            "uses: dtolnay/rust-toolchain@stable\n        with:\n          components: clippy\n"
        ),
        "{workflow}"
    );
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
