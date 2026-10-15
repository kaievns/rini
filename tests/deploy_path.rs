//! `bin/sign-rustc.sh` refuses a compile into this checkout's `target/release` unless
//! `RINI_DEPLOY=1`, so a bare `cargo build --release` cannot replace the binary launchd runs. These
//! hand it a fake rustc and the out-dir cargo would, and never build anything.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

struct Run {
    output: Output,
    rustc_ran: bool,
}

fn wrap(out_dir: &Path, deploy: Option<&str>) -> Run {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let rustc = scratch.path().join("rustc");
    let marker = scratch.path().join("ran");
    fs::write(&rustc, format!("#!/bin/sh\n: > '{}'\n", marker.display())).expect("fake rustc");
    fs::set_permissions(&rustc, fs::Permissions::from_mode(0o755)).expect("executable");

    let mut command = std::process::Command::new(Path::new(ROOT).join("bin/sign-rustc.sh"));
    command
        .arg(&rustc)
        .args(["--crate-name", "probe", "--out-dir"])
        .arg(out_dir)
        .current_dir(scratch.path())
        .env_remove("RINI_DEPLOY");
    if let Some(value) = deploy {
        command.env("RINI_DEPLOY", value);
    }
    let output = command.output().expect("bin/sign-rustc.sh runs");
    Run {
        output,
        rustc_ran: marker.exists(),
    }
}

fn under_root(relative: &str) -> PathBuf { Path::new(ROOT).join(relative) }

fn assert_refused(run: &Run, out_dir: &Path) {
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    assert!(!run.output.status.success(), "{} was allowed", out_dir.display());
    assert!(
        !run.rustc_ran,
        "rustc ran before the refusal for {}",
        out_dir.display()
    );
    assert!(stderr.contains("CARGO_TARGET_DIR=target-dev"), "{stderr}");
    assert!(stderr.contains("RINI_DEPLOY=1"), "{stderr}");
}

fn assert_allowed(run: &Run, out_dir: &Path) {
    assert!(
        run.output.status.success(),
        "{} was refused: {}",
        out_dir.display(),
        String::from_utf8_lossy(&run.output.stderr)
    );
    assert!(run.rustc_ran, "rustc did not run for {}", out_dir.display());
}

#[test]
fn a_compile_into_the_checkouts_target_release_is_refused_before_rustc_runs() {
    for relative in [
        "target/release/deps",
        "target/release",
        "target/release/build/rini-0123/out",
    ] {
        let out_dir = under_root(relative);
        assert_refused(&wrap(&out_dir, None), &out_dir);
    }
}

#[test]
fn rini_deploy_other_than_one_still_refuses() {
    let out_dir = under_root("target/release/deps");
    for value in ["", "0", "yes"] {
        assert_refused(&wrap(&out_dir, Some(value)), &out_dir);
    }
}

#[test]
fn rini_deploy_one_compiles_into_target_release() {
    let out_dir = under_root("target/release/deps");
    assert_allowed(&wrap(&out_dir, Some("1")), &out_dir);
}

#[test]
fn every_other_target_dir_compiles() {
    for relative in [
        "target-dev/release/deps",
        "target/debug/deps",
        "target/aarch64-apple-darwin/release/deps",
        "target/release-extra/deps",
    ] {
        let out_dir = under_root(relative);
        assert_allowed(&wrap(&out_dir, None), &out_dir);
    }
}

#[test]
fn target_release_reached_through_a_symlink_is_refused() {
    let links = tempfile::tempdir().expect("scratch dir");
    let checkout = links.path().join("checkout");
    std::os::unix::fs::symlink(ROOT, &checkout).expect("symlink to the checkout");
    let out_dir = checkout.join("target/release/deps");
    assert_refused(&wrap(&out_dir, None), &out_dir);
}
