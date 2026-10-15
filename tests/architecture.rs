//! The tree's rules, checked against the tree.
//!
//! Ratchets: what already breaks a rule is named, and nothing else may join it. The files that break
//! the macOS rule are `MIXED`, the functions and files past the size limits `LONG_FNS` and
//! `LONG_FILES`. The rules in prose, why they are these rules, and why a test rather than the
//! compiler holds them: `docs/architecture.md`.

use std::fs;
use std::ops::RangeInclusive;
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
    code_lines_in(&fs::read_to_string(path).expect("readable source"))
}

fn code_lines_in(source: &str) -> Vec<(usize, String)> {
    blank_comments(source, false)
        .lines()
        .enumerate()
        .map(|(n, line)| (n + 1, line.trim().to_string()))
        .filter(|(_, line)| !line.is_empty())
        .collect()
}

#[test]
fn a_comment_is_stripped_and_a_comment_marker_in_a_string_is_not() {
    let source =
        "let url = \"http://x\"; // note\n/* a\n /* nested */ still */ let b = '/';\n// only\n";
    assert_eq!(code_lines_in(source), vec![
        (1, "let url = \"http://x\";".to_string()),
        (3, "let b = '/';".to_string())
    ]);
}

fn feature_files() -> Vec<PathBuf> {
    FEATURES
        .iter()
        .flat_map(|f| rust_files(Path::new("src").join(f).as_path()))
        .collect()
}

fn slash(path: &Path) -> String { path.to_string_lossy().replace('\\', "/") }

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

/// What `KeyRouter` owns and asks in order. A tap holding one of them decides a key event outside
/// the router, where no router test sees the order (src/input/domain/router.rs).
const KEY_RULES: [&str; 4] = [
    "HeldKeys",
    "SwitchSession",
    "HotkeyTable",
    "modifiers_satisfy",
];

/// Whether `code` names `name` whole, not as part of a longer identifier.
fn names_identifier(code: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    code.match_indices(name)
        .any(|(at, _)| !code[..at].ends_with(word) && !code[at + name.len()..].starts_with(word))
}

#[test]
fn an_identifier_is_named_only_whole() {
    assert!(names_identifier(
        "let s = SwitchSession::default();",
        "SwitchSession"
    ));
    assert!(!names_identifier(
        "Request::EndSwitchSession => {",
        "SwitchSession"
    ));
    assert!(!names_identifier("HeldKeysCache", "HeldKeys"));
}

#[test]
fn the_tap_decides_a_key_event_only_through_the_key_router() {
    let mut broken = Vec::new();
    for path in rust_files(Path::new("src/input/platform")) {
        let source = fs::read_to_string(&path).expect("readable source");
        for (line, code) in production_lines_in(&source) {
            for name in KEY_RULES {
                if names_identifier(&code, name) {
                    broken.push(format!("{}:{line}: {name} in {}", slash(&path), code.trim()));
                }
            }
        }
    }
    assert!(
        broken.is_empty(),
        "the tap reads a key event into a Keystroke and does what KeyRouter decides:\n{}",
        broken.join("\n")
    );
}

/// The flight engine asks the window server only through the `WindowServer` its `Parts` hand it and
/// captures only through `Parts::capture`, so its tests state every answer it reads.
#[test]
fn the_flight_engine_reads_the_window_server_only_through_its_port() {
    let mut broken = Vec::new();
    for path in rust_files(Path::new("src/transitions")) {
        let source = fs::read_to_string(&path).expect("readable source");
        for (line, code) in production_lines_in(&source) {
            let free = code
                .match_indices("window_server::")
                .any(|(at, path)| !code[at + path.len()..].starts_with("WindowServer"));
            if free || code.contains("skylight::") {
                broken.push(format!("{}:{line}: {}", slash(&path), code.trim()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "the flight engine reads through Parts::window_server and Parts::capture:\n{}",
        broken.join("\n")
    );
}

/// The reads and commands the `WindowServer` trait declares, by name.
fn window_server_port_names() -> Vec<String> {
    let source = fs::read_to_string("src/windows/platform/window_server/port.rs").expect("port.rs");
    let body = source
        .split_once("pub trait WindowServer")
        .and_then(|(_, rest)| rest.split_once("\n}\n"))
        .expect("the WindowServer trait")
        .0;
    code_lines_in(body)
        .into_iter()
        .filter_map(|(_, code)| {
            let name = code.strip_prefix("fn ")?.split(['(', '<']).next()?;
            Some(name.to_string())
        })
        .collect()
}

/// A free function named as one of the port's reads or commands is a way around the port: whatever
/// calls it reaches the window server with no fake a test could hand it. The window server module
/// offers none, so every caller holds an `Arc<dyn WindowServer>`.
#[test]
fn the_window_server_is_reached_only_through_its_port() {
    let names = window_server_port_names();
    assert!(names.iter().any(|name| name == "get_window"), "{names:?}");
    let mut files = vec![PathBuf::from("src/windows/platform/window_server.rs")];
    files.extend(rust_files(Path::new("src/windows/platform/window_server")));
    let mut broken = Vec::new();
    for path in &files {
        let source = fs::read_to_string(path).expect("readable source");
        for (n, line) in source.lines().enumerate() {
            let free = ["pub fn ", "pub(crate) fn ", "pub(super) fn "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix));
            let Some(name) = free.and_then(|rest| rest.split(['(', '<']).next()) else {
                continue;
            };
            if names.iter().any(|read| read == name) {
                broken.push(format!("{}:{}: {}", slash(path), n + 1, line.trim()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "call the port a caller is handed rather than a free function of the same name:\n{}",
        broken.join("\n")
    );
}

/// A test states the window server's answers on the `FakeWindowServer` it hands the code under test,
/// never through a process- or thread-wide override (docs/testing.md, "A unit test must not read the
/// live window server").
#[test]
fn no_test_states_an_answer_through_an_override() {
    let mut files = rust_files(Path::new("src"));
    files.extend(
        rust_files(Path::new("tests"))
            .into_iter()
            .filter(|path| !path.ends_with("architecture.rs")),
    );
    let mut broken = Vec::new();
    for path in &files {
        let source = fs::read_to_string(path).expect("readable source");
        for (line, code) in code_lines_in(&source) {
            if code.contains("_override(") {
                broken.push(format!("{}:{line}: {}", slash(path), code.trim()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "state the answer on the fake:\n{}",
        broken.join("\n")
    );
}

/// What the per-app thread would reach the platform through past its `AxWorld`. The world is what a
/// test fakes, so a call past it reads the live window server or posts a real key event
/// (src/windows/platform/ax/world.rs). A test may set `FakeAx`'s fields of these names, but takes
/// no path through either module: the types it needs come from the world too.
const PAST_THE_WORLD: [&str; 2] = ["window_server", "mouse"];

/// Whether `code` names a path through `module`, as `module::` with no identifier running into it.
fn names_path_through(code: &str, module: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    code.match_indices(&format!("{module}::"))
        .any(|(at, _)| !code[..at].ends_with(word))
}

#[test]
fn a_path_through_a_module_is_named_only_whole() {
    assert!(names_path_through(
        "use crate::windows::platform::mouse::MouseState;",
        "mouse"
    ));
    assert!(!names_path_through("let m = ax.mouse;", "mouse"));
    assert!(!names_path_through("fake_window_server::new()", "window_server"));
}

#[test]
fn the_app_actor_reaches_the_platform_only_through_its_world() {
    let path = Path::new("src/windows/platform/app_actor.rs");
    let source = fs::read_to_string(path).expect("readable source");
    let mut broken = Vec::new();
    for (line, code) in production_lines_in(&source) {
        for name in PAST_THE_WORLD {
            if names_identifier(&code, name) {
                broken.push(format!("{}:{line}: {name} in {}", slash(path), code.trim()));
            }
        }
    }
    for (line, code) in code_lines_in(&source) {
        for name in PAST_THE_WORLD {
            if names_path_through(&code, name) {
                broken.push(format!("{}:{line}: {name}:: in {}", slash(path), code.trim()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "the app actor asks the window server, the pointer state and Command-W of its AxWorld:\n{}",
        broken.join("\n")
    );
}

/// A module only a test builds: the 1-based lines of one written inline, or the name of one in a
/// file of its own.
#[derive(Debug, PartialEq)]
enum TestModule {
    Inline(RangeInclusive<usize>),
    File(String),
}

/// The modules `source` declares under `#[cfg(test)]`. rustfmt closes an inline module with a `}` at
/// the indent of its `mod`, so a raw string holding such a line can end one early, never late.
fn test_modules(source: &str) -> Vec<TestModule> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    let mut n = 0;
    while n < lines.len() {
        if lines[n].trim() != "#[cfg(test)]" {
            n += 1;
            continue;
        }
        let start = n;
        n += 1;
        while lines.get(n).is_some_and(|l| l.trim().starts_with(['#', '/'])) {
            n += 1;
        }
        let Some(item) = lines.get(n) else { break };
        let Some((name, inline)) = module_declared(item) else {
            continue;
        };
        if !inline {
            out.push(TestModule::File(name.to_string()));
            continue;
        }
        let close = format!("{}}}", &item[..item.len() - item.trim_start().len()]);
        let end = (n + 1..lines.len()).find(|&m| lines[m] == close).unwrap_or_else(|| {
            panic!("`mod {name}` at line {} has no `{close}` closing it", n + 1)
        });
        out.push(TestModule::Inline(start + 1..=end + 1));
        n = end + 1;
    }
    out
}

/// The name of the module `line` declares, and whether its body is inline.
fn module_declared(line: &str) -> Option<(&str, bool)> {
    let mut decl = line.trim();
    if let Some(rest) = decl.strip_prefix("pub") {
        let rest = rest.strip_prefix('(').and_then(|r| r.split_once(')')).map_or(rest, |(_, r)| r);
        decl = rest.trim_start();
    }
    let decl = decl.strip_prefix("mod ")?;
    match decl.strip_suffix(" {") {
        Some(name) => Some((name, true)),
        None => decl.strip_suffix(';').map(|name| (name, false)),
    }
}

/// The files among `files` that only a test builds: each one a `#[cfg(test)] mod x;` names, and
/// every file below it.
fn test_files(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for path in files {
        let source = fs::read_to_string(path).expect("readable source");
        let parent = path.parent().expect("a file has a directory");
        let stem = path.file_stem().expect("a file has a name");
        let dir = match stem.to_str() {
            Some("mod" | "lib" | "main") => parent.to_path_buf(),
            _ => parent.join(stem),
        };
        for module in test_modules(&source) {
            if let TestModule::File(name) = module {
                roots.push(dir.join(name));
            }
        }
    }
    files
        .iter()
        .filter(|f| roots.iter().any(|root| f.with_extension("") == *root || f.starts_with(root)))
        .cloned()
        .collect()
}

/// The 1-based lines of each inline `#[cfg(test)]` module of `source`.
fn inline_test_ranges(source: &str) -> Vec<RangeInclusive<usize>> {
    test_modules(source)
        .into_iter()
        .filter_map(|module| match module {
            TestModule::Inline(lines) => Some(lines),
            TestModule::File(_) => None,
        })
        .collect()
}

/// The code of `source` that builds outside a test, comments stripped: all of it but its inline
/// `#[cfg(test)]` modules.
fn production_lines_in(source: &str) -> Vec<(usize, String)> {
    let inline = inline_test_ranges(source);
    code_lines_in(source)
        .into_iter()
        .filter(|(line, _)| !inline.iter().any(|lines| lines.contains(line)))
        .collect()
}

/// rini's panic hook aborts before an unwind reaches a `catch_unwind`, so a catch outside a test is
/// code that never runs (specs/service.md).
#[test]
fn no_panic_is_caught_outside_a_test() {
    let files: Vec<PathBuf> = rust_files(Path::new("src"))
        .into_iter()
        .chain(rust_files(Path::new("crates")))
        .collect();
    let tests = test_files(&files);
    let mut broken = Vec::new();
    for path in files.iter().filter(|path| !tests.contains(path)) {
        let source = fs::read_to_string(path).expect("readable source");
        for (line, code) in production_lines_in(&source) {
            if code.contains("catch_unwind") {
                broken.push(format!("{}:{line}: {}", slash(path), code.trim()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "a panic aborts rini and launchd relaunches it, so these catches never run:\n{}",
        broken.join("\n")
    );
}

#[test]
fn only_a_cfg_test_module_is_left_out_of_the_production_lines() {
    let source = r#"#[cfg(test)]
use std::panic::catch_unwind;

#[cfg(test)]
fn helper() { catch_unwind(|| ()); }

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    fn inner() {
        catch_unwind(|| ());
    }
}

mod outer {
    #[cfg(test)]
    pub(crate) mod nested {
        fn inner() { catch_unwind(|| ()); }
    }

    fn production() { catch_unwind(|| ()); }
}

#[cfg(test)]
pub(crate) mod testing;
"#;
    let caught: Vec<usize> = production_lines_in(source)
        .into_iter()
        .filter(|(_, code)| code.contains("catch_unwind"))
        .map(|(line, _)| line)
        .collect();
    assert_eq!(caught, vec![2, 5, 21]);
    assert_eq!(test_modules(source), vec![
        TestModule::Inline(7..=13),
        TestModule::Inline(16..=19),
        TestModule::File("testing".to_string()),
    ]);
}

#[test]
fn a_file_cfg_test_names_and_every_file_below_it_are_test_files() {
    let files = rust_files(Path::new("src"));
    let tests = test_files(&files);
    for name in [
        "src/app/reactor/testing.rs",
        "src/app/reactor/tests/mod.rs",
        "src/app/reactor/tests/spaces.rs",
        "src/app/hotkeys/lower/tests.rs",
        "src/animation/domain/motion/testing.rs",
    ] {
        assert!(
            tests.contains(&PathBuf::from(name)),
            "{name} is not read as a test file"
        );
    }
    for name in [
        "src/app/reactor/mod.rs",
        "src/app/hotkeys/lower.rs",
        "src/animation/domain/motion.rs",
    ] {
        assert!(
            !tests.contains(&PathBuf::from(name)),
            "{name} is read as a test file"
        );
    }
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

/// The map at the top of `src/lib.rs` names every feature.
#[test]
fn the_crate_map_names_every_feature() {
    let map = fs::read_to_string("src/lib.rs").expect("readable src/lib.rs");
    let unnamed: Vec<&str> = FEATURES
        .iter()
        .chain(["app"].iter())
        .copied()
        .filter(|feature| !map.contains(&format!("//! src/{feature}/ ")))
        .collect();
    assert!(
        unnamed.is_empty(),
        "src/lib.rs's map of the tree leaves out: {unnamed:?}"
    );
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
    for (dir, extensions) in [
        (".", &["md"][..]),
        ("tools/focuslab", &["py", "sh", "swift"]),
    ] {
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
    assert_eq!(first_arguments(&lines, "SLSWindowQueryRun"), vec![
        (2, "*G_CONNECTION".to_string()),
        (3, "*READ_CONNECTION".to_string()),
        (6, "SLSMainConnectionID(".to_string()),
    ]);
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

/// The longest a function may be, in code lines: comments, blank lines and test code not counted.
const FN_LIMIT: usize = 100;
/// The longest a file may be, counted the same way.
const FILE_LIMIT: usize = 1500;

/// The functions over `FN_LIMIT`, each at its size now as its ceiling: file, name, ceiling.
const LONG_FNS: [(&str, &str, usize); 26] = [
    ("crates/rini-ipc/src/mach.rs", "mach_server_begin", 101),
    ("src/animation/domain/choreography/slide.rs", "retarget", 156),
    ("src/animation/domain/motion/plan.rs", "merge_plans", 202),
    ("src/app/config/kdl/apply.rs", "apply", 137),
    ("src/app/config/kdl/apply.rs", "apply_binds", 105),
    ("src/app/config/kdl/apply.rs", "window_rule", 124),
    ("src/displays/screen.rs", "refresh_snapshot", 106),
    ("src/input/platform/keyboard.rs", "build_cg_keycode_table", 125),
    ("src/layout/domain/constraints.rs", "solve_axis_lengths", 114),
    ("src/layout/domain/scrolling.rs", "calculate_layout", 243),
    ("src/layout/domain/scrolling.rs", "on_window_resized", 105),
    ("src/main.rs", "main", 287),
    ("src/pictures/domain/watch.rs", "seen", 151),
    ("src/transitions/platform/engine.rs", "merge_now", 120),
    ("src/transitions/platform/engine.rs", "start", 229),
    ("src/transitions/platform/engine.rs", "start_surface", 120),
    ("src/windows/platform/app_actor.rs", "handle_notification", 148),
    ("src/windows/platform/app_actor.rs", "handle_raise_request", 101),
    ("src/windows/platform/app_actor.rs", "handle_request", 132),
    (
        "src/workspaces/domain/virtual_workspace.rs",
        "apply_app_rule_decision",
        133,
    ),
    (
        "src/workspaces/engine.rs",
        "calculate_layout_with_virtual_workspaces",
        177,
    ),
    ("src/workspaces/engine.rs", "handle_event_inner", 123),
    ("src/workspaces/engine.rs", "move_focus_internal", 106),
    ("src/workspaces/engine.rs", "move_window_to_space", 118),
    ("src/workspaces/engine.rs", "move_window_to_workspace", 156),
    ("src/workspaces/engine.rs", "on_windows_on_screen_updated", 151),
];

/// The files over `FILE_LIMIT`, each at its size now as its ceiling: file, ceiling.
const LONG_FILES: [(&str, usize); 3] = [
    ("src/layout/domain/scrolling.rs", 1816),
    ("src/transitions/platform/engine.rs", 2187),
    ("src/workspaces/engine.rs", 2544),
];

/// `source` with every comment blanked, so a `//` inside a string is not a comment, and with
/// `literals` every string and character literal's contents replaced by `x` too, so a brace or a
/// `fn` inside one is not code. Lines stay where they were.
fn blank_comments(source: &str, literals: bool) -> String {
    let chars: Vec<char> = source.chars().collect();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(source.len());
    let keep_line = |c: char, fill: char| if c == '\n' { '\n' } else { fill };
    let literal = |c: char| if literals { keep_line(c, 'x') } else { c };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let before = i.checked_sub(1).map(|p| chars[p]);
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(keep_line(chars[i], ' '));
                    i += 1;
                }
            }
        } else if c == 'r'
            && matches!(next, Some('"' | '#'))
            && before.is_none_or(|b| !ident(b) || b == 'b')
        {
            let hashes = chars[i + 1..].iter().take_while(|&&h| h == '#').count();
            if chars.get(i + 1 + hashes) != Some(&'"') {
                out.push(c);
                i += 1;
                continue;
            }
            let close: String =
                std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
            let open = 2 + hashes;
            out.extend(&chars[i..i + open]);
            i += open;
            while i < chars.len() && !chars[i..].starts_with(&close.chars().collect::<Vec<_>>()) {
                out.push(literal(chars[i]));
                i += 1;
            }
            out.push_str(&close);
            i += close.len();
        } else if c == '"' {
            out.push('"');
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    out.push(literal('\\'));
                    i += 1;
                }
                if let Some(&escaped) = chars.get(i) {
                    out.push(literal(escaped));
                }
                i += 1;
            }
            out.push('"');
            i += 1;
        } else if c == '\'' && next == Some('\\') {
            out.push('\'');
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                if chars[i] == '\\' {
                    out.push(literal('\\'));
                    i += 1;
                }
                if let Some(&escaped) = chars.get(i) {
                    out.push(literal(escaped));
                }
                i += 1;
            }
            out.push('\'');
            i += 1;
        } else if c == '\'' && chars.get(i + 2) == Some(&'\'') {
            out.extend(['\'', literal(chars[i + 1]), '\'']);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// A function in a source file: its name, the 1-based line of its `fn`, and its code lines from
/// that line to its closing brace.
#[derive(Debug, PartialEq)]
struct FnSpan {
    name: String,
    line: usize,
    code_lines: usize,
}

/// What of `source` builds outside a test: its code-line count and every function with a body,
/// neither counting an inline `#[cfg(test)]` module or an item `#[cfg(test)]` or `#[test]` marks.
fn production_size(source: &str) -> (usize, Vec<FnSpan>) {
    let blanked = blank_comments(source, true);
    let lines: Vec<&str> = blanked.lines().collect();
    let mut excluded = inline_test_ranges(source);
    let is_code = |n: usize| !lines[n - 1].trim().is_empty();
    let mut fns = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        let Some((name, column)) = fns_declared(line).next() else {
            continue;
        };
        if excluded.iter().any(|lines| lines.contains(&number)) {
            continue;
        }
        let Some(end) = body_end(&lines, index, column) else {
            continue;
        };
        if let Some(first) = test_attribute(&lines, index) {
            excluded.push(first + 1..=end + 1);
            continue;
        }
        let code_lines = (number..=end + 1).filter(|&n| is_code(n)).count();
        fns.push(FnSpan { name, line: number, code_lines });
    }
    let total = (1..=lines.len())
        .filter(|&n| is_code(n) && !excluded.iter().any(|lines| lines.contains(&n)))
        .count();
    (total, fns)
}

/// The names the `fn`s on `line` declare, each with the column after it.
fn fns_declared(line: &str) -> impl Iterator<Item = (String, usize)> + '_ {
    let bytes = line.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    line.match_indices("fn ").filter_map(move |(start, _)| {
        if start > 0 && ident(bytes[start - 1]) {
            return None;
        }
        let rest = line[start + 3..].trim_start();
        let name: String = rest.bytes().take_while(|&b| ident(b)).map(char::from).collect();
        (!name.is_empty() && !name.as_bytes()[0].is_ascii_digit())
            .then(|| (name, line.len() - rest.len()))
    })
}

/// The 0-based line of the brace closing the body of the `fn` that starts at `lines[index]`, or
/// `None` for a declaration with no body.
fn body_end(lines: &[&str], index: usize, column: usize) -> Option<usize> {
    let mut nesting = 0i32;
    let mut depth = 0i32;
    let mut opened = false;
    for (n, line) in lines.iter().enumerate().skip(index) {
        let text = if n == index { &line[column..] } else { line };
        for b in text.bytes() {
            match b {
                b'(' | b'[' => nesting += 1,
                b')' | b']' => nesting -= 1,
                b';' if !opened && nesting == 0 => return None,
                b'{' if opened || nesting == 0 => {
                    opened = true;
                    depth += 1;
                }
                b'}' if opened => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(n);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// The 0-based line of the first attribute above `lines[index]`, when one of them marks its item
/// as built only for a test.
fn test_attribute(lines: &[&str], index: usize) -> Option<usize> {
    let mut first = index;
    let mut marked = false;
    for n in (0..index).rev() {
        let line = lines[n].trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with("#[") {
            break;
        }
        first = n;
        marked |= line == "#[cfg(test)]" || line == "#[test]";
    }
    marked.then_some(first)
}

/// Every non-test file of `src/` and `crates/`, with its production size.
fn production_sizes() -> Vec<(String, usize, Vec<FnSpan>)> {
    let files: Vec<PathBuf> = rust_files(Path::new("src"))
        .into_iter()
        .chain(rust_files(Path::new("crates")))
        .collect();
    let tests = test_files(&files);
    files
        .iter()
        .filter(|path| !tests.contains(path))
        .map(|path| {
            let (total, fns) = production_size(&fs::read_to_string(path).expect("readable source"));
            (slash(path), total, fns)
        })
        .collect()
}

/// What breaks the size limits in `sizes`, given the functions and files named with a ceiling: an
/// offender not named, a named one past its ceiling or under it, and a named one back under the
/// limit or gone. A ceiling names the longest function of its name in its file; any other of that
/// name is held to `FN_LIMIT`.
fn size_breaches(
    sizes: &[(String, usize, Vec<FnSpan>)],
    long_fns: &[(&str, &str, usize)],
    long_files: &[(&str, usize)],
) -> Vec<String> {
    let mut broken = Vec::new();
    for (file, total, fns) in sizes {
        match long_files.iter().find(|(name, _)| name == file) {
            Some(_) if *total <= FILE_LIMIT => broken.push(format!(
                "{file}: {total} code lines, under {FILE_LIMIT}: remove it from LONG_FILES"
            )),
            Some(&(_, ceiling)) if *total > ceiling => {
                broken.push(format!(
                    "{file}: {total} code lines, past its ceiling of {ceiling}"
                ));
            }
            Some(&(_, ceiling)) if *total < ceiling => broken.push(format!(
                "{file}: {total} code lines, under its ceiling of {ceiling}: lower it in \
                 LONG_FILES to {total}"
            )),
            None if *total > FILE_LIMIT => {
                broken.push(format!("{file}: {total} code lines, past {FILE_LIMIT}"));
            }
            _ => {}
        }
        for (index, span) in fns.iter().enumerate() {
            let longest = fns
                .iter()
                .enumerate()
                .filter(|(_, other)| other.name == span.name)
                .max_by_key(|&(at, other)| (other.code_lines, std::cmp::Reverse(at)))
                .map(|(at, _)| at);
            let entry = long_fns.iter().find(|(f, name, _)| f == file && *name == span.name);
            match entry.filter(|_| longest == Some(index)) {
                Some(_) if span.code_lines <= FN_LIMIT => broken.push(format!(
                    "{file}: `{}` {} code lines, under {FN_LIMIT}: remove it from LONG_FNS",
                    span.name, span.code_lines
                )),
                Some(&(_, _, ceiling)) if span.code_lines > ceiling => broken.push(format!(
                    "{file}:{}: `{}` {} code lines, past its ceiling of {ceiling}",
                    span.line, span.name, span.code_lines
                )),
                Some(&(_, _, ceiling)) if span.code_lines < ceiling => broken.push(format!(
                    "{file}:{}: `{}` {} code lines, under its ceiling of {ceiling}: lower it in \
                     LONG_FNS to {}",
                    span.line, span.name, span.code_lines, span.code_lines
                )),
                None if span.code_lines > FN_LIMIT => broken.push(format!(
                    "{file}:{}: `{}` {} code lines, past {FN_LIMIT}",
                    span.line, span.name, span.code_lines
                )),
                _ => {}
            }
        }
    }
    for &(file, name, _) in long_fns {
        let present = sizes
            .iter()
            .any(|(f, _, fns)| f == file && fns.iter().any(|span| span.name == name));
        if !present {
            broken.push(format!("{file}: `{name}` is gone: remove it from LONG_FNS"));
        }
    }
    for &(file, _) in long_files {
        if !sizes.iter().any(|(f, _, _)| f == file) {
            broken.push(format!("{file} is gone: remove it from LONG_FILES"));
        }
    }
    broken.sort();
    broken
}

/// No function grows past `FN_LIMIT` code lines and no file past `FILE_LIMIT`. The ones already
/// past are named in `LONG_FNS` and `LONG_FILES` with a ceiling at their size now: one that shrinks
/// lowers its ceiling in the same change, and leaves the list once it is back under.
#[test]
fn no_function_or_file_grows_past_its_limit() {
    let broken = size_breaches(&production_sizes(), &LONG_FNS, &LONG_FILES);
    assert!(
        broken.is_empty(),
        "split the function or the file rather than raising a ceiling or adding an entry, and lower \
         a ceiling the change shrank under:\n{}",
        broken.join("\n")
    );
}

#[test]
fn a_size_breach_is_a_new_offender_a_grown_exception_or_a_stale_one() {
    let span = |name: &str, line, code_lines| FnSpan {
        name: name.to_string(),
        line,
        code_lines,
    };
    let sizes = vec![
        ("a.rs".to_string(), 1501, vec![
            span("new", 1, 101),
            span("grown", 2, 121),
            span("kept", 3, 115),
            span("lowered", 4, 110),
            span("shrunk", 5, 100),
            span("twin", 6, 125),
            span("twin", 7, 105),
        ]),
        ("b.rs".to_string(), 1600, vec![span("small", 1, 100)]),
        ("c.rs".to_string(), 1500, vec![]),
        ("e.rs".to_string(), 1650, vec![]),
    ];
    let long_fns = [
        ("a.rs", "grown", 120),
        ("a.rs", "kept", 115),
        ("a.rs", "lowered", 115),
        ("a.rs", "shrunk", 130),
        ("a.rs", "twin", 125),
        ("a.rs", "deleted", 140),
    ];
    let long_files = [
        ("b.rs", 1700),
        ("c.rs", 1600),
        ("d.rs", 1600),
        ("e.rs", 1650),
    ];
    assert_eq!(size_breaches(&sizes, &long_fns, &long_files), vec![
        "a.rs: 1501 code lines, past 1500",
        "a.rs: `deleted` is gone: remove it from LONG_FNS",
        "a.rs: `shrunk` 100 code lines, under 100: remove it from LONG_FNS",
        "a.rs:1: `new` 101 code lines, past 100",
        "a.rs:2: `grown` 121 code lines, past its ceiling of 120",
        "a.rs:4: `lowered` 110 code lines, under its ceiling of 115: lower it in LONG_FNS to 110",
        "a.rs:7: `twin` 105 code lines, past 100",
        "b.rs: 1600 code lines, under its ceiling of 1700: lower it in LONG_FILES to 1600",
        "c.rs: 1500 code lines, under 1500: remove it from LONG_FILES",
        "d.rs is gone: remove it from LONG_FILES",
    ]);
}

#[test]
fn a_function_is_measured_in_code_lines_from_its_fn_to_its_closing_brace() {
    let source = r##"fn short() {
    let s = "{ not a brace";
    let c = '{';
    let r = r#"fn fake() {"#;
}

pub(crate) fn longer<'a>(a: [u8; 2], b: &'a str) -> u8 {
    // a comment line
    let x = 1; /* { */

    x + a[0]
}

trait T {
    fn declared(&self);
}

#[cfg(test)]
#[allow(dead_code)]
fn helper() {
    let x = 1;
}

#[cfg(test)]
mod tests {
    fn inside() {}
}
"##;
    let (total, fns) = production_size(source);
    assert_eq!(fns, vec![
        FnSpan {
            name: "short".to_string(),
            line: 1,
            code_lines: 5
        },
        FnSpan {
            name: "longer".to_string(),
            line: 7,
            code_lines: 4
        },
    ]);
    assert_eq!(total, 12);
}

/// Whether `text` names `path` whole, not as the tail of a longer path or name.
fn names_path(text: &str, path: &str) -> bool {
    let part = |c: char| c.is_alphanumeric() || c == '_' || c == '/';
    text.match_indices(path).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(part)
            && !text[at + path.len()..].chars().next().is_some_and(part)
    })
}

/// Every module of a feature's `domain/`, and of the reactor and its `events/`, has a line in the
/// README beside it, named by its path from the feature's root.
#[test]
fn every_domain_and_reactor_module_is_named_in_its_readme() {
    let mut layers: Vec<(PathBuf, PathBuf)> = FEATURES
        .iter()
        .map(|feature| (Path::new("src").join(feature), PathBuf::from("domain")))
        .collect();
    layers.push((PathBuf::from("src/app"), PathBuf::from("reactor")));
    layers.push((PathBuf::from("src/app"), PathBuf::from("reactor/events")));
    let mut missing = Vec::new();
    for (root, layer) in layers {
        let Ok(entries) = fs::read_dir(root.join(&layer)) else {
            continue;
        };
        let readme_path = root.join("docs/README.md");
        let readme = fs::read_to_string(&readme_path).expect("every feature has a README");
        let mut files: Vec<PathBuf> = entries
            .map(|entry| entry.expect("readable entry").path())
            .filter(|path| path.extension().is_some_and(|e| e == "rs"))
            .filter(|path| path.file_name().is_some_and(|name| name != "mod.rs"))
            .collect();
        files.sort();
        for path in files {
            let relative = slash(path.strip_prefix(&root).expect("under its root"));
            if !names_path(&readme, &relative) {
                missing.push(format!("{}: `{relative}`", slash(&readme_path)));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "each of these modules wants a row in its README's \"What it owns\", by its path from the \
         feature's root:\n{}",
        missing.join("\n")
    );
}

#[test]
fn a_path_is_named_only_whole() {
    let readme = "| `domain/app_rules.rs` | rules |\nsee src/x/domain/zoom.rs and domain/raise.rs.";
    assert!(names_path(readme, "domain/app_rules.rs"));
    assert!(names_path(readme, "domain/raise.rs"));
    assert!(!names_path(readme, "domain/rules.rs"));
    assert!(!names_path(readme, "domain/zoom.rs"));
    assert!(!names_path(readme, "domain/raise_order.rs"));
}

/// Names the docs quote that rini does not define: the window server's and SkyLight's internals as
/// disassembled, yabai's and niri's functions as read from their source, and rustc's and clippy's
/// lints.
const OTHERS_NAMES: [&str; 16] = [
    "managed_display_set_current_space",
    "post_space_lifecycle_notification",
    "managed_space_update_membership",
    "sls_transaction_commit_ca",
    "hw_capture_window_list_common",
    "window_list_create_image_from_array_internal",
    "window_manager_animate_window_list_async",
    "scripting_addition_swap_window_proxy_in",
    "scripting_addition_swap_window_proxy_out",
    "window_manager_set_window_frame",
    "sls_window_disable_shadow",
    "is_centering_focused_column",
    "compute_new_view_offset",
    "allow_attributes_without_reason",
    "unsafe_op_in_unsafe_fn",
    "fn_params_excessive_bools",
];

/// Names the docs record as deleted: the audits' findings as they were found, and rift's options
/// a spec says are gone.
const GONE_NAMES: [&str; 14] = [
    "focus_stays_on_this_display",
    "current_reported_space_for_window_id",
    "capture_composite_via_skylight",
    "broadcast_window_title_changed",
    "broadcast_focused_window_changed",
    "last_space_for_display_uuid",
    "space_for_display_uuid",
    "workspace_step_at_boundary",
    "should_buffer_topology_updates",
    "should_quarantine_window_space_event",
    "consume_or_expel_window",
    "focus_follows_mouse_disable_hotkey",
    "mouse_hides_on_focus",
    "resolve_menu_bar_space",
];

/// The snake_case name of four words or more an inline code span `span` holds, path and call
/// parentheses aside: `a_b_c_d`, `module::a_b_c_d`, `a_b_c_d()`.
fn long_snake_name(span: &str) -> Option<&str> {
    let path = span.strip_suffix("()").unwrap_or(span);
    let (prefix, name) = path.rsplit_once("::").unwrap_or(("", path));
    let identifier = |part: &str| {
        part.starts_with(|c: char| c.is_alphabetic() || c == '_')
            && part.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    let words: Vec<&str> = name.split('_').collect();
    let snake = words.len() >= 4
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && words.iter().all(|word| {
            !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    (snake && (prefix.is_empty() || prefix.split("::").all(identifier))).then_some(name)
}

/// The inline code spans of `text` outside its fenced blocks, each with its 1-based line.
fn code_spans(text: &str) -> Vec<(usize, &str)> {
    let mut fenced = false;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        out.extend(line.split('`').skip(1).step_by(2).map(|span| (n + 1, span)));
    }
    out
}

/// The names `source` declares as a function or a field.
fn declared_names(source: &str) -> Vec<String> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    for (_, code) in code_lines_in(source) {
        out.extend(fns_declared(&code).map(|(name, _)| name));
        let field = code.trim_start();
        let field = field.strip_prefix("pub").map_or(field, |rest| {
            let rest =
                rest.strip_prefix('(').and_then(|r| r.split_once(')')).map_or(rest, |(_, r)| r);
            rest.trim_start()
        });
        if let Some((name, _)) = field.split_once(':')
            && !name.is_empty()
            && name.chars().all(ident)
            && !field[name.len()..].starts_with("::")
        {
            out.push(name.to_string());
        }
    }
    out
}

/// The markdown the identifier check reads: the root README, `docs/`, `specs/`, and every feature's
/// and crate's own `docs/`.
fn documentation() -> Vec<PathBuf> {
    let mut files = vec![PathBuf::from("README.md")];
    for dir in ["docs", "specs"] {
        collect_markdown(Path::new(dir), &mut files);
    }
    let mut inside = Vec::new();
    for dir in ["src", "crates"] {
        collect_markdown(Path::new(dir), &mut inside);
    }
    files.extend(inside.into_iter().filter(|path| slash(path).contains("/docs/")));
    files.sort();
    files
}

/// Every function a doc names by a snake_case name of four words or more, the tests included, is
/// still a function or a field in `src/`, `crates/` or `tests/`, so renaming a test a doc cites fails
/// here. The names rini quotes from other code are `OTHERS_NAMES`, and the ones a doc records as
/// deleted are `GONE_NAMES`.
#[test]
fn every_long_name_a_doc_cites_is_still_defined() {
    let mut defined = std::collections::HashSet::new();
    for path in ["src", "crates", "tests"].iter().flat_map(|dir| rust_files(Path::new(dir))) {
        defined.extend(declared_names(
            &fs::read_to_string(&path).expect("readable source"),
        ));
    }
    let mut broken = Vec::new();
    let mut cited = std::collections::HashSet::new();
    for path in documentation() {
        let text = fs::read_to_string(&path).expect("readable doc");
        for (line, span) in code_spans(&text) {
            let Some(name) = long_snake_name(span) else { continue };
            cited.insert(name.to_string());
            let listed = OTHERS_NAMES.contains(&name) || GONE_NAMES.contains(&name);
            if !defined.contains(name) && !listed {
                broken.push(format!("{}:{line}: `{name}`", slash(&path)));
            }
        }
    }
    for name in OTHERS_NAMES.iter().chain(GONE_NAMES.iter()) {
        if defined.contains(*name) {
            broken.push(format!("`{name}` is defined in rini: remove it from its list"));
        } else if !cited.contains(*name) {
            broken.push(format!("`{name}` is cited by no doc: remove it from its list"));
        }
    }
    assert!(
        broken.is_empty(),
        "these name a function or a field rini no longer has. Name the one that holds it now, or \
         record it as gone:\n{}",
        broken.join("\n")
    );
}

#[test]
fn a_long_snake_name_is_read_whole_from_its_span() {
    assert_eq!(
        long_snake_name("every_spec_is_listed"),
        Some("every_spec_is_listed")
    );
    assert_eq!(long_snake_name("reactor::query::a_b_c_d()"), Some("a_b_c_d"));
    assert_eq!(long_snake_name("three_word_name"), None);
    assert_eq!(long_snake_name("WAITS_ON_THE_MAIN"), None);
    assert_eq!(long_snake_name("src/a_b_c_d.rs"), None);
    assert_eq!(long_snake_name("a_b_c_d(x)"), None);
    assert_eq!(long_snake_name("a__b_c_d"), None);
    let doc = "`one_two_three_four` and `x`\n```\n`five_six_seven_eight`\n```\nlast `y`";
    assert_eq!(code_spans(doc), vec![
        (1, "one_two_three_four"),
        (1, "x"),
        (5, "y")
    ]);
    let source = "pub(crate) fn a_b_c_d() {}\nstruct S {\n    pub e_f_g_h: u8,\n    i_j_k_l: u8,\n}\n// fn m_n_o_p\nuse a::b;\nfn q_r_s_t(f: fn(u8)) {} fn u_v_w_x() {}";
    assert_eq!(declared_names(source), vec![
        "a_b_c_d", "e_f_g_h", "i_j_k_l", "q_r_s_t", "u_v_w_x"
    ]);
}
