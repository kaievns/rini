//! Running programs: the config's `spawn-at-startup` and `spawn-sh-at-startup`, and the `spawn`
//! and `spawn-sh` actions.
use serde::Serialize;
use tracing::{error, trace};

/// A command the config asks rini to run once at launch.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum StartupCommand {
    /// A program and its arguments, as written.
    Spawn(Vec<String>),
    /// One command line for `sh -c`.
    SpawnSh(String),
}

impl StartupCommand {
    pub fn argv(&self) -> Vec<String> {
        match self {
            StartupCommand::Spawn(argv) => argv.clone(),
            StartupCommand::SpawnSh(command) => shell(command),
        }
    }
}

/// The argv that hands `command` to `sh -c`, so pipes, `$VARIABLES` and quoting work.
pub fn shell(command: &str) -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), command.to_owned()]
}

pub fn execute_startup_commands(commands: &[StartupCommand]) {
    for command in commands {
        spawn(command.argv());
    }
}

/// Runs `argv` on a thread of its own, logging a failure or a non-zero exit with its output.
pub fn spawn(argv: Vec<String>) {
    std::thread::spawn(move || {
        let [program, args @ ..] = argv.as_slice() else {
            error!("asked to run an empty command");
            return;
        };
        trace!(?argv, "running");
        match std::process::Command::new(program).args(args).output() {
            Ok(output) if output.status.success() => trace!(?argv, "finished"),
            Ok(output) => {
                error!(?argv, status = %output.status, "command failed");
                error!("stdout: {}", String::from_utf8_lossy(&output.stdout));
                error!("stderr: {}", String::from_utf8_lossy(&output.stderr));
            }
            Err(e) => error!(?argv, "could not run: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_runs_its_arguments_as_written() {
        let command = StartupCommand::Spawn(vec!["open".into(), "-a".into(), "My App".into()]);
        assert_eq!(command.argv(), ["open", "-a", "My App"]);
    }

    #[test]
    fn spawn_sh_hands_the_whole_line_to_the_shell() {
        let command = StartupCommand::SpawnSh("echo \"$HOME\" | wc -c".into());
        assert_eq!(command.argv(), ["/bin/sh", "-c", "echo \"$HOME\" | wc -c"]);
    }
}
