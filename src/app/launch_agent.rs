// ported from https://github.com/koekeishiya/yabai/blob/master/src/misc/service.h

use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::Subcommand;
use nix::unistd::getuid;

const LAUNCHCTL_PATH: &str = "/bin/launchctl";
const RINI_PLIST: &str = "git.kaievns.rini";

#[derive(Subcommand)]
pub enum ServiceCommands {
    /// Install the per-user launchd service
    Install,
    /// Uninstall the per-user launchd service
    Uninstall,
    /// Start (or bootstrap) the service
    Start {
        /// Move the service to this binary when it runs one at another path
        #[arg(long = "move")]
        allow_move: bool,
    },
    /// Stop (or bootout/kill) the service
    Stop,
    /// Restart the service (kickstart -k)
    Restart {
        /// Move the service to this binary when it runs one at another path
        #[arg(long = "move")]
        allow_move: bool,
    },
}

pub fn handle_service_command(cmd: &ServiceCommands) -> Result<&'static str, String> {
    match cmd {
        ServiceCommands::Install => service_install()
            .map(|_| "Service installed.")
            .map_err(|e| format!("Failed to install service: {}", e)),
        ServiceCommands::Uninstall => service_uninstall()
            .map(|_| "Service uninstalled.")
            .map_err(|e| format!("Failed to uninstall service: {}", e)),
        ServiceCommands::Start { allow_move } => service_start(*allow_move)
            .map(|_| "Service started.")
            .map_err(|e| format!("Failed to start service: {}", e)),
        ServiceCommands::Stop => service_stop()
            .map(|_| "Service stopped.")
            .map_err(|e| format!("Failed to stop service: {}", e)),
        ServiceCommands::Restart { allow_move } => service_restart(*allow_move)
            .map(|_| "Service restarted.")
            .map_err(|e| format!("Failed to restart service: {}", e)),
    }
}

fn plist_path() -> io::Result<PathBuf> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "HOME not set"))?;
    Ok(home.join("Library").join("LaunchAgents").join(format!("{RINI_PLIST}.plist")))
}

/// The binary the service should run: the one running this command, symlinks resolved.
///
/// Not a `$PATH` lookup. A lookup found a stale `~/.local/bin/rini` and launched a month-old build under
/// a code requirement TCC had never granted; the build asked to start the service is the build meant.
/// Resolved because TCC keys the grant to the launch path, and a symlink behaves as an ungranted client.
/// Both are measured in `docs/permissions-and-the-launch-agent.md`.
fn agent_executable(invoked: &Path) -> PathBuf {
    // A failure here means the link is dangling, in which case the path as written is the best
    // available answer and launchd will report the real problem.
    invoked.canonicalize().unwrap_or_else(|_| invoked.to_path_buf())
}

fn find_rini_executable() -> io::Result<PathBuf> {
    let invoked = env::current_exe().map_err(|_| {
        io::Error::new(
            io::ErrorKind::Other,
            "unable to retrieve path of current executable",
        )
    })?;
    Ok(agent_executable(&invoked))
}

/// What launchd has to be told to get the job running the plist on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launch {
    /// Not loaded: load it and start it.
    Bootstrap,
    /// Loaded from a plist that has since changed: unload, load again, start.
    Reload,
    /// Loaded and current: start it, killing a running instance first when `kill`.
    Kickstart { kill: bool },
}

/// launchd runs the job definition it LOADED, not the file on disk. So a plist that changed while the
/// job is loaded only takes effect after a bootout and a fresh bootstrap — a kickstart, with or without
/// `-k`, would run the old binary again, which is how a rewritten plist could still leave a stale build
/// running.
fn launch_plan(loaded: bool, plist_changed: bool, restart: bool) -> Launch {
    match (loaded, plist_changed) {
        (false, _) => Launch::Bootstrap,
        (true, true) => Launch::Reload,
        (true, false) => Launch::Kickstart { kill: restart },
    }
}

fn launch(plan: Launch, plist_path: &Path) -> io::Result<()> {
    let uid = getuid();
    let service_target = format!("gui/{}/{}", uid, RINI_PLIST);
    let domain_target = format!("gui/{}", uid);
    let plist = plist_path.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "service file path is not UTF-8")
    })?;
    match plan {
        Launch::Bootstrap => {
            let _ = run_launchctl(&["enable", &service_target], true);
            let _ = spawn_launchctl(&["bootstrap", &domain_target, plist]);
        }
        Launch::Reload => {
            let _ = run_launchctl(&["bootout", &domain_target, plist], true);
            let _ = spawn_launchctl(&["bootstrap", &domain_target, plist]);
        }
        Launch::Kickstart { .. } => {}
    }
    if matches!(plan, Launch::Bootstrap | Launch::Reload) {
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    let args: &[&str] = match plan {
        Launch::Kickstart { kill: true } => &["kickstart", "-k", &service_target],
        _ => &["kickstart", &service_target],
    };
    let code = run_launchctl(args, false)?;
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("{plan:?}: kickstart failed (exit {code})"),
        ))
    }
}

/// Point the plist at the build running this command. True when that changed it.
///
/// Skipped without `USER` and `PATH` in the environment, which the plist is built from; an existing
/// plist is then left as it is rather than rewritten with gaps.
fn refresh_plist(plist_path: &Path, allow_move: bool) -> io::Result<bool> {
    if env::var_os("USER").is_some() && env::var_os("PATH").is_some() {
        ensure_plist_up_to_date(plist_path, allow_move)
    } else {
        Ok(false)
    }
}

/// The binary a plist written by `plist_xml` runs.
fn program_in_plist(plist: &str) -> Option<&str> {
    let after_key = &plist[plist.find("<key>ProgramArguments</key>")?..];
    let start = after_key.find("<string>")? + "<string>".len();
    let len = after_key[start..].find("</string>")?;
    Some(&after_key[start..start + len])
}

/// Refuses to move the service to a binary at another path unless asked to.
///
/// macOS ties Accessibility and Screen Recording to the binary's path as well as its signature, so a
/// move costs a re-grant of both even for a build signed the same way. See `docs/signing.md`.
fn check_binary_move(
    installed: Option<&str>,
    invoked: &str,
    allow_move: bool,
) -> Result<(), String> {
    match installed {
        Some(installed) if installed != invoked && !allow_move => Err(format!(
            "the service runs {installed}, and this binary is {invoked}. macOS ties Accessibility \
             and Screen Recording to the binary's path, so moving the service means granting both \
             again. Run `{installed} service ...` instead, or pass --move to move it."
        )),
        _ => Ok(()),
    }
}

fn service_is_loaded() -> bool {
    let service_target = format!("gui/{}/{}", getuid(), RINI_PLIST);
    run_launchctl(&["print", &service_target], true).unwrap_or(1) == 0
}

fn plist_contents() -> io::Result<String> {
    let user =
        env::var("USER").map_err(|_| io::Error::new(io::ErrorKind::Other, "env USER not set"))?;
    let path_env =
        env::var("PATH").map_err(|_| io::Error::new(io::ErrorKind::Other, "env PATH not set"))?;

    let agent_exe = find_rini_executable()?;
    let exe_str = agent_exe
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "non-UTF8 executable path"))?;

    Ok(plist_xml(exe_str, &path_env, &user))
}

/// Builds the launch agent plist.
///
/// Separated from reading the environment so the document can be checked without a rini on `PATH`.
fn plist_xml(exe: &str, path_env: &str, user: &str) -> String {
    format!(
        // Raw string: escaping the quotes writes literal backslashes into the plist, and
        // `plutil -lint` accepts that. Asserted by `the_plist_quotes_attributes_without_backslashes`.
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{name}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path_env}</string>
        <key>RUST_LOG</key>
        <string>error,warn,info</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
        <key>Crashed</key>
        <true/>
    </dict>
    <key>StandardOutPath</key>
    <string>/tmp/rini_{user}.out.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/rini_{user}.err.log</string>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>Nice</key>
    <integer>-20</integer>
</dict>
</plist>
"#,
        name = RINI_PLIST,
        exe = exe,
        path_env = path_env,
        user = user
    )
}
/*<key>MachServices</key>
<dict>
    <key>{name}</key>
    <true/>
</dict> */

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn write_file_atomic(path: &Path, contents: &str) -> io::Result<()> {
    ensure_parent_dir(path)?;
    let mut f = File::create(path)?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}

fn run_launchctl(args: &[&str], suppress_output: bool) -> io::Result<i32> {
    let mut cmd = Command::new(LAUNCHCTL_PATH);
    cmd.args(args);
    if suppress_output {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let status = cmd.status()?;

    if let Some(code) = status.code() {
        Ok(code)
    } else {
        let sig = status.signal().unwrap_or_default();
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("launchctl terminated by signal {}", sig),
        ))
    }
}

fn spawn_launchctl(args: &[&str]) -> io::Result<()> {
    let mut cmd = Command::new(LAUNCHCTL_PATH);
    cmd.args(args);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let _child = cmd.spawn()?;
    Ok(())
}

fn service_is_running() -> io::Result<bool> {
    let uid = getuid();
    let service_target = format!("gui/{}/{}", uid, RINI_PLIST);
    match run_launchctl(&["print", &service_target], true) {
        Ok(code) => Ok(code == 0),
        Err(_) => Ok(false),
    }
}

pub fn service_install_internal(plist_path: &Path) -> io::Result<()> {
    let plist = plist_contents()?;
    write_file_atomic(plist_path, &plist)?;
    Ok(())
}

fn ensure_plist_up_to_date(plist_path: &Path, allow_move: bool) -> io::Result<bool> {
    let desired = plist_contents()?;
    match fs::read_to_string(plist_path) {
        Ok(existing) if existing == desired => Ok(false),
        Ok(existing) => {
            let invoked = program_in_plist(&desired).unwrap_or_default();
            check_binary_move(program_in_plist(&existing), invoked, allow_move)
                .map_err(|message| io::Error::new(io::ErrorKind::PermissionDenied, message))?;
            write_file_atomic(plist_path, &desired)?;
            Ok(true)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            write_file_atomic(plist_path, &desired)?;
            Ok(true)
        }
        Err(err) => Err(err),
    }
}

pub fn service_install() -> io::Result<()> {
    let plist_path = plist_path()?;
    if plist_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("service file '{}' is already installed", plist_path.display()),
        ));
    }
    service_install_internal(&plist_path)
}

pub fn service_uninstall() -> io::Result<()> {
    let plist_path = plist_path()?;
    if !plist_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("service file '{}' is not installed", plist_path.display()),
        ));
    }
    if service_is_running()? {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "service is still running; stop it first with `rini service stop` before uninstalling",
        ));
    }
    fs::remove_file(plist_path)?;
    Ok(())
}

/// Start the service on the build running this command.
pub fn service_start(allow_move: bool) -> io::Result<()> {
    let plist_path = plist_path()?;
    if !plist_path.is_file() {
        service_install_internal(&plist_path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "service file '{}' could not be installed: {}",
                    plist_path.display(),
                    e
                ),
            )
        })?;
    }
    let plist_changed = refresh_plist(&plist_path, allow_move)?;
    launch(
        launch_plan(service_is_loaded(), plist_changed, false),
        &plist_path,
    )
}

/// Restart the service on the build running this command.
pub fn service_restart(allow_move: bool) -> io::Result<()> {
    let plist_path = plist_path()?;
    if !plist_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("service file '{}' is not installed", plist_path.display()),
        ));
    }
    let plist_changed = refresh_plist(&plist_path, allow_move)?;
    launch(
        launch_plan(service_is_loaded(), plist_changed, true),
        &plist_path,
    )
}

pub fn service_stop() -> io::Result<()> {
    let plist_path = plist_path()?;
    if !plist_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("service file '{}' is not installed", plist_path.display()),
        ));
    }

    let uid = getuid();
    let service_target = format!("gui/{}/{}", uid, RINI_PLIST);
    let domain_target = format!("gui/{}", uid);

    let is_bootstrapped = run_launchctl(&["print", &service_target], true).unwrap_or(1);

    if is_bootstrapped != 0 {
        let code = run_launchctl(&["kill", "SIGTERM", &service_target], false)?;
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                format!("kill SIGTERM failed (exit {})", code),
            ))
        }
    } else {
        let code1 =
            run_launchctl(&["bootout", &domain_target, plist_path.to_str().unwrap()], false)?;
        let code2 = run_launchctl(&["disable", &service_target], false)?;

        if code1 == 0 && code2 == 0 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                format!("bootout exit {}, disable exit {}", code1, code2),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs as unix_fs;

    use super::*;

    /// TCC keys the grant to the launch path, so a symlinked invocation must point the service at the
    /// real file.
    #[test]
    fn the_agent_runs_the_real_file_behind_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let real = dir.join("rini-real");
        fs::write(&real, b"#!/bin/sh\nexit 0\n").unwrap();
        let link = dir.join("rini");
        unix_fs::symlink(&real, &link).unwrap();

        assert_eq!(agent_executable(&link), real.canonicalize().unwrap());
    }

    /// A dangling link is passed through as written, so launchd reports the real problem.
    #[test]
    fn a_dangling_link_is_passed_through() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("rini");
        unix_fs::symlink(tmp.path().join("gone"), &link).unwrap();

        assert_eq!(agent_executable(&link), link);
    }

    /// The case that left a stale build running: a plist rewritten while the job is loaded has to be
    /// reloaded, for a start AND a restart, because launchd runs what it loaded.
    #[test]
    fn a_changed_plist_under_a_loaded_job_is_reloaded() {
        assert_eq!(launch_plan(true, true, false), Launch::Reload);
        assert_eq!(launch_plan(true, true, true), Launch::Reload);
    }

    #[test]
    fn an_unloaded_job_is_bootstrapped_whatever_changed() {
        assert_eq!(launch_plan(false, true, false), Launch::Bootstrap);
        assert_eq!(launch_plan(false, false, true), Launch::Bootstrap);
    }

    /// A start leaves a running instance alone; a restart kills it first.
    #[test]
    fn a_current_plist_is_just_kickstarted() {
        assert_eq!(
            launch_plan(true, false, false),
            Launch::Kickstart { kill: false }
        );
        assert_eq!(launch_plan(true, false, true), Launch::Kickstart { kill: true });
    }

    #[test]
    fn the_plist_quotes_attributes_without_backslashes() {
        let plist = plist_xml("/Users/x/.local/bin/rini", "/usr/bin", "x");
        assert!(
            !plist.contains('\\'),
            "no backslash belongs anywhere in this document"
        );
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(plist.contains("<plist version=\"1.0\">"));
    }

    /// The reported case: a rollback pointed the service at a build in another directory, and every
    /// such move cost a re-grant. Refused unless asked for, naming both paths.
    #[test]
    fn a_binary_at_another_path_does_not_take_over_the_service_unasked() {
        let installed = "/Users/k/projects/rini/target/release/rini";
        let other = "/Users/k/projects/rini/target-3ad0d60/release/rini";

        let refused = check_binary_move(Some(installed), other, false).unwrap_err();
        assert!(refused.contains(installed) && refused.contains(other));
        assert!(refused.contains("--move"));
        assert_eq!(check_binary_move(Some(installed), other, true), Ok(()));
    }

    /// Refused before anything is written: the plist still runs the binary it did.
    #[test]
    fn a_refused_move_leaves_the_installed_plist_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let plist_path = tmp.path().join("git.kaievns.rini.plist");
        let installed = plist_xml("/somewhere/else/rini", "/usr/bin", "kai");
        fs::write(&plist_path, &installed).unwrap();

        let refused = ensure_plist_up_to_date(&plist_path, false).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&plist_path).unwrap(), installed);

        assert!(ensure_plist_up_to_date(&plist_path, true).unwrap());
        let moved = fs::read_to_string(&plist_path).unwrap();
        assert_ne!(program_in_plist(&moved), Some("/somewhere/else/rini"));
    }

    /// The same path is not a move, and a plist naming no binary has none to protect.
    #[test]
    fn the_same_binary_or_no_installed_one_is_not_a_move() {
        let rini = "/Users/k/projects/rini/target/release/rini";
        assert_eq!(check_binary_move(Some(rini), rini, false), Ok(()));
        assert_eq!(check_binary_move(None, rini, false), Ok(()));
    }

    #[test]
    fn the_installed_binary_is_read_back_from_the_plist() {
        let plist = plist_xml("/opt/homebrew/bin/rini", "/usr/bin:/bin", "kai");
        assert_eq!(program_in_plist(&plist), Some("/opt/homebrew/bin/rini"));
        assert_eq!(program_in_plist("<plist></plist>"), None);
    }

    #[test]
    fn the_plist_carries_the_executable_and_user_through() {
        let plist = plist_xml("/opt/homebrew/bin/rini", "/usr/bin:/bin", "kai");
        assert!(plist.contains("<string>/opt/homebrew/bin/rini</string>"));
        assert!(plist.contains("<string>/usr/bin:/bin</string>"));
        assert!(plist.contains("/tmp/rini_kai.out.log"));
        assert!(plist.contains("/tmp/rini_kai.err.log"));
    }
}
