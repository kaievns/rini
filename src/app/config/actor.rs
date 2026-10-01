use std::path::PathBuf;

use rini_runloop::channel as channels;
use tracing::{debug, error, info};

use crate::app::config::{Config, ConfigCommand};

pub type Sender = channels::Sender<Event>;
pub type Receiver = channels::Receiver<Event>;

#[derive(Debug)]
pub enum Event {
    QueryConfig(r#continue::Sender<Config>),
    ApplyConfig {
        cmd: ConfigCommand,
        response: r#continue::Sender<Result<(), String>>,
    },
}

/// What the config thread tells the rest of rini.
#[derive(Debug)]
pub enum Change {
    /// A change was accepted: a fragment applied, or the file read again.
    Applied(Config),
    /// The file was read again, and whether it could be used. One that could not left the running
    /// config as it was.
    FileRead { usable: bool },
}

/// Called on the config thread.
pub type OnChange = Box<dyn Fn(Change) + Send>;

pub struct ConfigActor {
    config: Config,
    on_change: OnChange,
    config_path: PathBuf,
}

impl ConfigActor {
    pub fn spawn_with_path(config: Config, on_change: OnChange, config_path: PathBuf) -> Sender {
        let (tx, rx) = channels::channel();
        std::thread::Builder::new()
            .name("config".to_string())
            .spawn(move || {
                let actor = ConfigActor { config, on_change, config_path };
                rini_runloop::executor::Executor::run(actor.run(rx));
            })
            .unwrap();
        tx
    }

    async fn run(mut self, mut events: Receiver) {
        while let Some((_span, event)) = events.recv().await {
            match event {
                Event::QueryConfig(resp) => {
                    let _ = resp.send(self.config.clone());
                }
                Event::ApplyConfig { cmd, response } => {
                    let res = self.handle_config_command(cmd);
                    let _ = response.send(res);
                }
            }
        }
    }

    /// One command against the running config. A change that cannot be used changes nothing, and the
    /// error carries the whole report.
    fn handle_config_command(&mut self, cmd: ConfigCommand) -> Result<(), String> {
        debug!("Applying config command: {:?}", cmd);
        match cmd {
            ConfigCommand::GetConfig => Ok(()),
            ConfigCommand::Set(fragment) => {
                let changed =
                    self.config.with("config set", &fragment).map_err(|error| error.render())?;
                self.accept(changed);
                Ok(())
            }
            ConfigCommand::ReloadConfig => match Config::read(&self.config_path) {
                Ok(config) => {
                    self.accept(config);
                    (self.on_change)(Change::FileRead { usable: true });
                    Ok(())
                }
                Err(error) => {
                    let report = error.render();
                    error!(
                        "the config was not reloaded; rini keeps running on the last good one\n{report}"
                    );
                    (self.on_change)(Change::FileRead { usable: false });
                    Err(report)
                }
            },
        }
    }

    fn accept(&mut self, config: Config) {
        info!("config changed");
        self.config = config;
        (self.on_change)(Change::Applied(self.config.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(path: PathBuf) -> (ConfigActor, std::sync::mpsc::Receiver<Change>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let actor = ConfigActor {
            config: Config::default(),
            on_change: Box::new(move |config| tx.send(config).unwrap()),
            config_path: path,
        };
        (actor, rx)
    }

    #[test]
    fn a_fragment_changes_the_running_config_and_says_so() {
        let (mut actor, changes) = actor(PathBuf::from("/nonexistent"));
        actor
            .handle_config_command(ConfigCommand::Set("layout { gaps 8; }".into()))
            .expect("applies");
        assert_eq!(actor.config.settings.layout.gaps.gaps, 8.0);
        let Ok(Change::Applied(announced)) = changes.try_recv() else {
            panic!("announced")
        };
        assert_eq!(announced.settings.layout.gaps.gaps, 8.0);
        assert!(changes.try_recv().is_err(), "a fragment is not the file");
        assert_eq!(
            actor.config.binds,
            Config::default().binds,
            "everything the fragment leaves out stays"
        );
    }

    #[test]
    fn a_bad_fragment_changes_nothing_and_reports_why() {
        let (mut actor, changes) = actor(PathBuf::from("/nonexistent"));
        let before = actor.config.clone();
        let error = actor
            .handle_config_command(ConfigCommand::Set("layout { gaps \"wide\"; }".into()))
            .unwrap_err();
        assert!(error.contains("expected a number"), "{error}");
        assert_eq!(actor.config, before);
        assert!(changes.try_recv().is_err(), "nothing announced");
    }

    #[test]
    fn a_failed_reload_keeps_the_running_config_and_carries_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.kdl");
        std::fs::write(&path, "binds {\n    Mod+J { focus-colum-left; }\n}\n").unwrap();
        let (mut actor, changes) = actor(path.clone());
        let before = actor.config.clone();

        let error = actor.handle_config_command(ConfigCommand::ReloadConfig).unwrap_err();
        assert!(error.contains("did you mean `focus-column-left`?"), "{error}");
        assert_eq!(actor.config, before);
        assert!(matches!(
            changes.try_recv(),
            Ok(Change::FileRead { usable: false })
        ));
        assert!(changes.try_recv().is_err(), "nothing applied");

        std::fs::write(&path, "layout { gaps 5; }\n").unwrap();
        actor
            .handle_config_command(ConfigCommand::ReloadConfig)
            .expect("a good file loads");
        assert_eq!(actor.config.settings.layout.gaps.gaps, 5.0);
        assert!(matches!(changes.try_recv(), Ok(Change::Applied(_))));
        assert!(matches!(
            changes.try_recv(),
            Ok(Change::FileRead { usable: true })
        ));
    }
}
