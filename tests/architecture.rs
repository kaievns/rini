//! The tree's rules, checked against the tree.
//!
//! A ratchet: the files that already break the macOS rule are named in `MIXED`, and nothing else may
//! join them. The rules in prose, why they are these rules, and why a test rather than the compiler
//! holds them: `docs/architecture.md`.

use std::fs;
use std::path::{Path, PathBuf};

const FEATURES: [&str; 11] = [
    "windows",
    "displays",
    "layout",
    "workspaces",
    "input",
    "decorations",
    "pictures",
    "animation",
    "transitions",
    "switcher",
    "bar",
];

/// macOS and FFI surfaces a pure module must not reach for. `objc2_core_foundation` is absent on
/// purpose for the features not yet on rini-geometry's spaces: there `CGRect`, `CGPoint` and `CGSize`
/// are the arithmetic every layout decision is in, and `rini-geometry` wraps them. The features that
/// are on the spaces are `TYPED`, and name no CG geometry outside `platform/`. `rini_skylight_sys` is
/// absent too, but only for its plain value types — see `SKYLIGHT_VALUE_TYPES`.
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
const SKYLIGHT_VALUE_TYPES: [&str; 4] = [
    "SpaceId",
    "WindowServerId",
    "DisplayReconfigFlags",
    "KnownCGSEvent",
];

/// The features whose interfaces and `domain/` speak rini-geometry's spaces (`space` in
/// `crates/rini-geometry`). CG geometry is their boundary, not their arithmetic.
const TYPED: [&str; 4] = ["decorations", "pictures", "animation", "transitions"];

/// CG geometry, and the conversions into it and out of it, that a typed feature leaves to its
/// `platform/`. Bare names, so a conversion called, passed as a path or imported as a trait all count.
const CG_GEOMETRY: [&str; 8] = [
    "objc2_core_foundation",
    "CGRect",
    "CGPoint",
    "CGSize",
    "to_cg",
    "from_cg",
    "ToCg",
    "FromCg",
];

/// The one place a size or a displacement crosses point spaces: `Respace`'s two impls, each a
/// `fn respace` line in this file.
const RESPACE: &str = "crates/rini-geometry/src/space.rs";

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
    FEATURES
        .iter()
        .flat_map(|f| rust_files(Path::new("src").join(f).as_path()))
        .collect()
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
fn a_typed_feature_names_no_cg_geometry_outside_platform() {
    let mut broken = Vec::new();
    for feature in TYPED {
        for path in rust_files(&Path::new("src").join(feature)) {
            let name = slash(&path);
            if name.contains("/platform/") {
                continue;
            }
            for (line, code) in code_lines(&path) {
                for cg in CG_GEOMETRY {
                    if code.contains(cg) {
                        broken.push(format!("{name}:{line}: {cg} in {}", code.trim()));
                    }
                }
            }
        }
    }
    for path in rust_files(Path::new("src")).into_iter().chain(rust_files(Path::new("crates"))) {
        let name = slash(&path);
        for (line, code) in code_lines(&path) {
            let respace = name == RESPACE && code.starts_with("fn respace");
            if code.contains("cast_unit") && !respace {
                broken.push(format!("{name}:{line}: {}", code.trim()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "a typed feature speaks rini-geometry's spaces: CG stays in its `platform/`, which converts \
         a value on the line where it crosses into or out of an interface or `domain/`. A size or a \
         displacement crosses point spaces through `Respace`, never `cast_unit`. See \
         crates/rini-geometry/docs/README.md:\n{}",
        broken.join("\n")
    );
}

/// What only the main thread may touch, which `transitions` runs off it: Core Animation, AppKit, the
/// overlay that holds both, and the renderer's read of the media clock, which the engine is handed.
const MAIN_THREAD_ONLY: [&str; 8] = [
    "objc2_quartz_core",
    "objc2_app_kit",
    "MainThreadMarker",
    "MainThreadOnly",
    "TileOverlay",
    "CATransaction",
    "CACurrentMediaTime",
    "media_clock",
];

#[test]
fn transitions_never_touches_core_animation_or_app_kit() {
    let mut broken = Vec::new();
    for path in rust_files(Path::new("src/transitions")) {
        let name = slash(&path);
        for (line, code) in code_lines(&path) {
            for surface in MAIN_THREAD_ONLY {
                if code.contains(surface) {
                    broken.push(format!("{name}:{line}: {surface} in {}", code.trim()));
                }
            }
        }
    }
    assert!(
        broken.is_empty(),
        "the flight engine runs on the `transitions` thread, and the overlay on the main one: the \
         engine reaches it only through `Draw`, and reads the media clock it is handed. See \
         \"Threads\" in src/transitions/docs/coordination.md:\n{}",
        broken.join("\n")
    );
}

/// What tapping or reading the trackpad takes: a tap at the HID location, the multitouch frames,
/// the touches on an NSEvent. specs/input.md: rini MUST NOT tap trackpad gestures, so the session
/// tap is the one tap there is, and only `EventTap` creates it.
const TRACKPAD: [&str; 4] = [
    "HIDEventTap",
    "AnnotatedSessionEventTap",
    "MultitouchSupport",
    "NSTouch",
];
const THE_TAP: &str = "src/input/platform/tap.rs";

#[test]
fn rini_taps_no_trackpad_gesture() {
    let mut broken = Vec::new();
    for path in rust_files(Path::new("src")).into_iter().chain(rust_files(Path::new("crates"))) {
        let name = slash(&path);
        for (line, code) in code_lines(&path) {
            for surface in TRACKPAD {
                if code.contains(surface) {
                    broken.push(format!("{name}:{line}: {surface} in {}", code.trim()));
                }
            }
            let creates = code.contains("tap_create") || code.contains("CGEventTapCreate");
            if creates && name != THE_TAP {
                broken.push(format!("{name}:{line}: a second tap in {}", code.trim()));
            }
        }
    }
    for name in ["build.rs", "Cargo.toml"] {
        if name == "build.rs" && !Path::new(name).exists() {
            continue;
        }
        let text = fs::read_to_string(name).expect("readable build file");
        for (n, line) in text.lines().enumerate() {
            for surface in TRACKPAD {
                if line.contains(surface) {
                    broken.push(format!("{name}:{}: {surface} in {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        broken.is_empty(),
        "rini MUST NOT tap trackpad gestures (specs/input.md): a swipe does what macOS does with \
         it, and the session tap {THE_TAP} creates is the one tap:\n{}",
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

/// Every feature and every crate carries its own `docs/README.md`.
///
/// The convention is in `docs/architecture.md` under "Where documentation lives". It is a test
/// because a new feature is exactly when nobody remembers it.
#[test]
fn every_feature_and_crate_documents_itself() {
    let mut missing = Vec::new();
    for feature in FEATURES.iter().chain(["app"].iter()) {
        let readme = Path::new("src").join(feature).join("docs/README.md");
        if !readme.is_file() {
            missing.push(slash(&readme));
        }
    }
    for entry in fs::read_dir("crates").expect("readable crates directory") {
        let crate_dir = entry.expect("readable entry").path();
        if !crate_dir.is_dir() {
            continue;
        }
        let readme = crate_dir.join("docs/README.md");
        if !readme.is_file() {
            missing.push(slash(&readme));
        }
    }
    assert!(
        missing.is_empty(),
        "a module explains itself beside its code. See \"Where documentation lives\" in \
         docs/architecture.md:\n{}",
        missing.join("\n")
    );
}

/// Every path a doc or a comment points at still exists.
///
/// Docs rotted three times before this: paths survived two renames of the tree and pointed at
/// modules that had moved, which is worse than no pointer because it reads as current.
#[test]
fn every_documented_path_resolves() {
    let mut broken = Vec::new();
    let mut check = |text: &str, source: &str| {
        for candidate in text.split(['`', '(', ')', ' ', '\n', ',', ';', '"']) {
            let candidate = candidate.trim_end_matches(['.', ':', '\'']);
            let is_path = ["src/", "crates/", "tests/", "docs/", "specs/", "tools/"]
                .iter()
                .any(|root| candidate.starts_with(root))
                && (candidate.ends_with('/')
                    || [
                        ".rs", ".md", ".toml", ".kdl", ".py", ".sh", ".swift", ".json", ".jsonl",
                        ".csv", ".txt",
                    ]
                    .iter()
                    .any(|extension| candidate.ends_with(extension)));
            // `<feature>` in a diagram is not a path, and a crate's own docs are named relative
            // to its root, so `docs/x.md` inside `crates/y/` means `crates/y/docs/x.md`.
            if !is_path || candidate.contains('<') {
                continue;
            }
            let crate_relative = source
                .split_once("/src/")
                .map(|(crate_root, _)| PathBuf::from(crate_root).join(candidate));
            if Path::new(candidate).exists() || crate_relative.is_some_and(|path| path.exists()) {
                continue;
            }
            broken.push(format!("{source}: {candidate}"));
        }
    };
    // This file's own paths are examples of what it checks.
    let mut files: Vec<PathBuf> = ["src", "crates", "tests"]
        .iter()
        .flat_map(|dir| rust_files(Path::new(dir)))
        .filter(|path| path != Path::new("tests/architecture.rs"))
        .collect();
    // The reference config is read by users, and points them at the docs.
    files.push(PathBuf::from("rini.default.kdl"));
    for dir in ["docs", "specs", "src", "crates", "tools"] {
        collect_markdown(Path::new(dir), &mut files);
    }
    for (dir, extensions) in [(".", &["md"][..]), ("tools/focuslab", &["py", "sh", "swift"])] {
        for entry in fs::read_dir(dir).expect("readable directory") {
            let path = entry.expect("readable entry").path();
            if path.extension().is_some_and(|e| extensions.iter().any(|x| e == *x)) {
                files.push(path.strip_prefix("./").unwrap_or(&path).to_path_buf());
            }
        }
    }
    for path in files {
        let text = fs::read_to_string(&path).expect("readable file");
        check(&text, &slash(&path));
    }
    broken.sort();
    broken.dedup();
    assert!(
        broken.is_empty(),
        "these point at files that do not exist. A stale pointer reads as a current one:\n{}",
        broken.join("\n")
    );
}

/// An Accessibility element is never handed to a log macro.
///
/// `AXUIElement`'s `Debug` hands off to the Core Foundation description, which QUERIES the element
/// for its role and title. Formatting one is therefore a round-trip to the application, and on a
/// wedged application it blocks — in a log line, which is the last place anyone looks for a stall.
///
/// This was found half-obeyed: `app_actor`'s `trace` had the field commented out with a FIXME, and
/// the error arm three lines below still formatted the element, in the branch reached when the
/// application is hung. A test rather than a comment, because the comment did not hold.
#[test]
fn no_accessibility_element_is_ever_logged() {
    let mut offenders = Vec::new();
    for path in rust_files(Path::new("src")) {
        let text = fs::read_to_string(&path).expect("readable source");
        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or(line);
            let names_an_element = ["elem", "element", "app_elem"].iter().any(|name| {
                code.contains(&format!("?{name}"))
                    || code.contains(&format!("{{{name}:"))
                    || code.contains(&format!("{{{name}}}"))
            });
            let is_a_log = [
                "trace!", "debug!", "info!", "warn!", "error!", "println!", "format!",
            ]
            .iter()
            .any(|macro_name| code.contains(macro_name));
            if names_an_element && is_a_log {
                offenders.push(format!("{}:{}  {}", slash(&path), number + 1, code.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "formatting an Accessibility element queries the application, so a log line becomes a \
         round-trip that blocks on a hung app. Log the window id instead:\n{}",
        offenders.join("\n")
    );
}

/// The SkyLight reads that wait on the last Core Animation transaction of the connection they are
/// given, found by disassembly (`docs/data/2026-10-08-skylight-lock/callgraph.txt`).
const WAITS_ON_CORE_ANIMATION: [&str; 4] = [
    "SLSWindowQueryWindows",
    "SLSWindowQueryRun",
    "SLSHWCaptureWindowList",
    "SLSCaptureWindowsContentsToRectWithOptions",
];

/// The first argument of every call to `name` in `lines`, which may run across lines.
fn first_arguments(lines: &[(usize, String)], name: &str) -> Vec<(usize, String)> {
    let text: Vec<&str> = lines.iter().map(|(_, code)| code.as_str()).collect();
    let mut found = Vec::new();
    for (i, (line, code)) in lines.iter().enumerate() {
        let mut from = 0;
        while let Some(at) = code[from..].find(&format!("{name}(")) {
            let start = from + at + name.len() + 1;
            from = start;
            if code[..start].trim_end_matches(&format!("{name}(")).trim_end().ends_with("fn") {
                continue;
            }
            let rest = std::iter::once(&code[start..]).chain(text[i + 1..].iter().copied());
            let joined: String = rest.collect::<Vec<_>>().join(" ");
            let end = joined.find([',', ')']).unwrap_or(joined.len());
            found.push((*line, joined[..end].trim().to_string()));
        }
    }
    found
}

/// Every read that waits on Core Animation is made on rini's read connection.
///
/// On the main connection such a read takes the connection's lock and waits for the last transaction
/// a commit of rini's own windows entangled, while the main thread's commit waits for the lock: both
/// stall 500ms, until SkyLight times the wait out. See "Reads go on a connection of their own" in
/// `src/windows/docs/README.md`.
#[test]
fn a_read_that_waits_on_core_animation_is_made_on_the_read_connection() {
    let mut broken = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in rust_files(Path::new("src")) {
        let lines = code_lines(&path);
        for name in WAITS_ON_CORE_ANIMATION {
            for (line, argument) in first_arguments(&lines, name) {
                seen.insert(name);
                if argument != "*READ_CONNECTION" {
                    broken.push(format!("{}:{line}: {name}({argument}, ..)", slash(&path)));
                }
            }
        }
    }
    let unseen: Vec<&str> = WAITS_ON_CORE_ANIMATION
        .iter()
        .copied()
        .filter(|name| !seen.contains(name))
        .collect();
    assert!(
        unseen.is_empty(),
        "no call to these is found any more, so this test checks nothing of them. Drop them from \
         WAITS_ON_CORE_ANIMATION:\n{}",
        unseen.join("\n")
    );
    assert!(
        broken.is_empty(),
        "these wait on the connection's last Core Animation transaction, so on rini's main connection \
         they deadlock with the main thread's commit for 500ms. Pass `*READ_CONNECTION` itself, not a \
         copy of it. See \"Reads go on a connection of their own\" in src/windows/docs/README.md:\n{}",
        broken.join("\n")
    );
}

/// The reads that wait on the main connection's last Core Animation transaction whatever
/// connection they are given, or that take none (`docs/data/2026-10-08-skylight-lock/callgraph.txt`,
/// `CGWindowList`'s as `SLWindowList*`).
const WAITS_ON_THE_MAIN_CONNECTION: [&str; 5] = [
    "CGWindowListCopyWindowInfo",
    "CGWindowListCreateImage",
    "SLSGetOnScreenWindowList",
    "SLSWindowIsOrderedIn",
    "SLSCopyWindowsWithOptionsAndTags",
];

/// No read is made that waits on the main connection whatever it is given.
///
/// It stalls with the main thread's commit for 500ms as a read on the main connection does. Each has
/// a read on the read connection that answers the same; see "Which reads wait, and on which
/// connection" in `src/windows/docs/window-server-reads.md`.
#[test]
fn no_read_waits_on_the_main_connection_whatever_it_is_given() {
    let mut broken = Vec::new();
    for path in rust_files(Path::new("src")).into_iter().chain(rust_files(Path::new("crates"))) {
        for (line, code) in code_lines(&path) {
            for name in WAITS_ON_THE_MAIN_CONNECTION {
                if code.contains(name) {
                    broken.push(format!("{}:{line}: {}", slash(&path), code.trim()));
                }
            }
        }
    }
    assert!(
        broken.is_empty(),
        "these wait on the main connection's last Core Animation transaction whatever connection \
         they are given. Read on `READ_CONNECTION` instead (`on_screen_windows`, `every_window`, \
         `window_ordered_in`, `space_window_list_for_connection`, `capture_rect`). See \"Which reads \
         wait, and on which connection\" in src/windows/docs/window-server-reads.md:\n{}",
        broken.join("\n")
    );
}

#[test]
fn a_first_argument_is_read_across_lines_and_never_from_a_declaration() {
    let lines: Vec<(usize, String)> = [
        "pub fn SLSWindowQueryRun(cid: c_int, query: *mut CFType) -> *mut CFType;",
        "let a = unsafe { SLSWindowQueryRun(*G_CONNECTION, query, 0) };",
        "SLSWindowQueryRun(",
        "*READ_CONNECTION,",
        "query)",
        "SLSWindowQueryRun(SLSMainConnectionID())",
    ]
    .iter()
    .enumerate()
    .map(|(n, code)| (n + 1, code.to_string()))
    .collect();
    assert_eq!(
        first_arguments(&lines, "SLSWindowQueryRun"),
        vec![
            (2, "*G_CONNECTION".to_string()),
            (3, "*READ_CONNECTION".to_string()),
            (6, "SLSMainConnectionID(".to_string()),
        ]
    );
}

/// Every spec is reachable from the index, and the index names nothing that is missing.
///
/// A spec nobody links to is a spec nobody reads, and `specs/README.md` is the only way in. The rule
/// that keeps them current is in that file; this is the part a test can hold.
#[test]
fn every_spec_is_listed_in_the_index() {
    let index = fs::read_to_string("specs/README.md").expect("specs/README.md exists");

    let mut unlisted = Vec::new();
    for entry in fs::read_dir("specs").expect("readable specs directory") {
        let path = entry.expect("readable entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == "README.md" || !name.ends_with(".md") {
            continue;
        }
        if !index.contains(name) {
            unlisted.push(name.to_owned());
        }
    }
    unlisted.sort();
    assert!(
        unlisted.is_empty(),
        "these specs are not in specs/README.md, so nothing leads to them:\n  {}",
        unlisted.join("\n  ")
    );
}

/// Every requirement is written as an obligation.
///
/// A spec that describes the code instead of stating what is required reads as a summary, and a
/// summary does not tell anyone whether a change is allowed. Each spec has to contain at least one
/// MUST, MUST NOT or MAY.
#[test]
fn every_spec_states_obligations_rather_than_describing_the_code() {
    let mut toothless = Vec::new();
    for entry in fs::read_dir("specs").expect("readable specs directory") {
        let path = entry.expect("readable entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == "README.md" || !name.ends_with(".md") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("readable spec");
        if !["MUST", "MAY"].iter().any(|word| text.contains(word)) {
            toothless.push(name.to_owned());
        }
    }
    toothless.sort();
    assert!(
        toothless.is_empty(),
        "these specs describe rather than require. State the obligation and its reason:\n  {}",
        toothless.join("\n  ")
    );
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries {
        let path = entry.expect("readable entry").path();
        if path.is_dir() {
            collect_markdown(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}
