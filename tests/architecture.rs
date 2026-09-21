//! The tree's rules, checked against the tree.
//!
//! A ratchet: the files that already break the macOS rule are named in `MIXED`, and nothing else may
//! join them. The rules in prose, why they are these rules, and why a test rather than the compiler
//! holds them: `docs/architecture.md`.

use std::fs;
use std::path::{Path, PathBuf};

const FEATURES: [&str; 6] = ["windows", "displays", "layout", "workspaces", "input", "animation"];

/// macOS and FFI surfaces a pure module must not reach for. `objc2_core_foundation` is absent on
/// purpose: `CGRect`, `CGPoint` and `CGSize` are the arithmetic every layout decision is in, and
/// `rini-geometry` wraps them. `rini_skylight_sys` is absent too, but only for its plain value types
/// — see `SKYLIGHT_VALUE_TYPES`.
const MACOS: [&str; 15] = [
    "objc2::",
    "objc2_app_kit",
    "objc2_application_services",
    "objc2_core_graphics",
    "objc2_core_media",
    "objc2_core_video",
    "objc2_foundation",
    "objc2_io_surface",
    "objc2_quartz_core",
    "objc2_screen_capture_kit",
    "rini_mach_sys",
    "dispatchr",
    "block2",
    "libc::",
    "extern \"C\"",
];

/// What the window server mints rather than what it does. A `SpaceId` is a number the domain has to
/// carry; `SLSSpaceGetType` is a call it must not make.
const SKYLIGHT_VALUE_TYPES: [&str; 5] = [
    "SpaceId",
    "WindowServerId",
    "DisplayReconfigFlags",
    "CGSEventType",
    "KnownCGSEvent",
];

/// Still on both sides of the domain/platform line: a pure model mixed with the reads that fill it.
/// `screen.rs` wants splitting on the `System` trait it is already generic over. Nothing may be added
/// here — split the file instead.
const MIXED: [&str; 1] = ["src/displays/screen.rs"];

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).expect("readable directory") {
        let path = entry.expect("readable entry").path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Lines with the comments stripped, so prose about `platform` is not a dependency on it.
fn code_lines(path: &Path) -> Vec<(usize, String)> {
    let source = fs::read_to_string(path).expect("readable source");
    let mut in_block = false;
    let mut out = Vec::new();
    for (n, raw) in source.lines().enumerate() {
        let mut line = raw.trim().to_string();
        if in_block {
            match line.find("*/") {
                Some(end) => {
                    line = line[end + 2..].to_string();
                    in_block = false;
                }
                None => continue,
            }
        }
        if let Some(start) = line.find("/*") {
            in_block = !line[start..].contains("*/");
            line.truncate(start);
        }
        if let Some(start) = line.find("//") {
            line.truncate(start);
        }
        if !line.trim().is_empty() {
            out.push((n + 1, line));
        }
    }
    out
}

fn feature_files() -> Vec<PathBuf> {
    FEATURES.iter().flat_map(|f| rust_files(Path::new("src").join(f).as_path())).collect()
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[test]
fn a_feature_never_names_the_application() {
    let mut broken = Vec::new();
    for path in feature_files() {
        for (line, code) in code_lines(&path) {
            if code.contains("crate::app") {
                broken.push(format!("{}:{line}: {}", slash(&path), code.trim()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "the application knows the features, not the other way round. A feature talks upward by \
         emitting its own `event` type, which the reactor converts:\n{}",
        broken.join("\n")
    );
}

#[test]
fn a_domain_never_reads_its_own_adapters() {
    let mut broken = Vec::new();
    for path in feature_files() {
        if !slash(&path).contains("/domain/") {
            continue;
        }
        for (line, code) in code_lines(&path) {
            if code.contains("platform::") {
                broken.push(format!("{}:{line}: {}", slash(&path), code.trim()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "`domain/` is the model and the decisions; `platform/` is the adapters. A type the domain \
         needs and an adapter fills is a port: declare it in `domain/` and let `platform/` import \
         it (`domain::info`, `domain::request`):\n{}",
        broken.join("\n")
    );
}

#[test]
fn nothing_outside_platform_touches_macos() {
    let mut broken = Vec::new();
    for path in feature_files() {
        let name = slash(&path);
        if name.contains("/platform/") || MIXED.contains(&name.as_str()) {
            continue;
        }
        for (line, code) in code_lines(&path) {
            for surface in MACOS {
                if code.contains(surface) {
                    broken.push(format!("{name}:{line}: {surface} in {}", code.trim()));
                }
            }
            if code.contains("rini_skylight_sys")
                && !SKYLIGHT_VALUE_TYPES.iter().any(|t| code.contains(t))
            {
                broken.push(format!("{name}:{line}: SkyLight API in {}", code.trim()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "macOS lives in `platform/`. A pure module compiles and tests without AppKit, which is the \
         whole return on this layout:\n{}",
        broken.join("\n")
    );
}

#[test]
fn every_named_exception_still_exists() {
    for name in MIXED {
        assert!(
            Path::new(name).exists(),
            "{name} is listed as a mixed domain/platform module but is gone. Delete the entry."
        );
    }
    for name in MIXED {
        let has_macos = code_lines(Path::new(name))
            .iter()
            .any(|(_, code)| MACOS.iter().any(|s| code.contains(s)));
        assert!(
            has_macos,
            "{name} no longer touches macOS, so it is no longer an exception. Delete the entry \
             and move the file into `domain/`."
        );
    }
}

#[test]
fn every_feature_and_its_layers_are_declared() {
    for feature in FEATURES {
        let dir = Path::new("src").join(feature);
        assert!(dir.join("mod.rs").is_file(), "src/{feature}/mod.rs is missing");
        for layer in ["domain", "platform"] {
            let layer_dir = dir.join(layer);
            if layer_dir.is_dir() {
                assert!(
                    layer_dir.join("mod.rs").is_file(),
                    "src/{feature}/{layer}/ has no mod.rs"
                );
            }
        }
    }
}
