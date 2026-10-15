//! `bin/check.sh`, the one command local runs and CI runs, driven against stub `cargo` and `rustup`
//! that record what they were asked: the steps, where they build and with which flags.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// A directory of stubs that log `CARGO_TARGET_DIR|RUSTFLAGS|<tool> <args>` a line per call, and
/// fail any call whose arguments contain `$FAIL`.
fn stubs(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rini-check-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for tool in ["cargo", "rustup"] {
        let path = dir.join(tool);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"$CARGO_TARGET_DIR|$RUSTFLAGS|{tool} $*\" >> \"$CHECK_LOG\"\n\
                 case \"$*\" in *\"$FAIL\"*) [ -n \"$FAIL\" ] && exit 1;; esac\nexit 0\n"
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
        .env_remove("RUSTFLAGS");
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
fn the_check_formats_builds_and_tests_into_target_dev_with_warnings_as_errors() {
    let (output, calls) = check(&stubs("pass"), &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = format!("{}/target-dev", fs::canonicalize(ROOT).unwrap().display());
    assert_eq!(calls, [
        format!("{target}|-D warnings|rustup run nightly cargo fmt --all --check"),
        format!("{target}|-D warnings|cargo build --locked --workspace --all-targets"),
        format!("{target}|-D warnings|cargo test --locked --workspace"),
    ]);
}

#[test]
fn a_failing_step_fails_the_check_and_every_other_step_still_runs() {
    let (output, calls) = check(&stubs("fail"), &[
        ("FAIL", "fmt"),
        ("CARGO_TARGET_DIR", "/elsewhere"),
        ("RUSTFLAGS", "-C debuginfo=0"),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed: fmt"));
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(
        calls
            .iter()
            .all(|call| call.starts_with("/elsewhere|-C debuginfo=0 -D warnings|")),
        "{calls:?}"
    );
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
