use std::cell::RefCell;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[cfg(test)]
use tempfile::NamedTempFile;
use tracing::Span;

use super::{Event, Reactor};
use crate::app::channels::{self};
use crate::app::config::Config;
use crate::windows::domain::request::{AppThreadHandle, Request};
use crate::workspaces::LayoutEngine;
use crate::workspaces::domain::display_memory::DisplayMemory;

thread_local! {
    static DESERIALIZE_THREAD_HANDLE: RefCell<Option<AppThreadHandle>> = RefCell::new(None);
}

pub(super) fn deserialize_app_thread_handle() -> AppThreadHandle {
    DESERIALIZE_THREAD_HANDLE
        .with(|handle| handle.borrow().clone().expect("No deserialize thread handle set!"))
}

pub struct Record {
    file: Option<File>,
    #[cfg(test)]
    temp: Option<NamedTempFile>,
}

impl Record {
    pub fn new(path: Option<&Path>) -> Self {
        Self {
            file: path.map(|path| File::create(path).unwrap()),
            #[cfg(test)]
            temp: None,
        }
    }

    #[cfg(test)]
    pub fn new_for_test(temp: NamedTempFile) -> Self {
        Self { file: None, temp: Some(temp) }
    }

    #[cfg(test)]
    #[allow(unused)]
    pub(super) fn temp(&mut self) -> Option<&mut NamedTempFile> {
        self.temp.as_mut()
    }

    fn file(&mut self) -> Option<&mut File> {
        #[cfg(test)]
        return self.file.as_mut().or(self.temp.as_mut().map(|temp| temp.as_file_mut()));
        #[cfg(not(test))]
        self.file.as_mut()
    }

    /// The recording opens with the layout; the config is the caller's to give `replay`, since a
    /// config is read from its file and has no other written form.
    pub(super) fn start(&mut self, layout: &LayoutEngine, memory: &DisplayMemory) {
        let Some(file) = self.file() else { return };
        let layout = layout.serialize_to_string(memory);
        write!(file, "{layout}\n").unwrap();
    }

    pub(super) fn on_event(&mut self, event: &Event) {
        let Some(file) = self.file() else { return };
        let Ok(line) = ron::ser::to_string(&event) else {
            return;
        };
        write!(file, "{line}\n").unwrap();
    }
}

pub fn replay(
    path: &Path,
    config: Config,
    mut on_event: impl FnMut(Span, Request) + Send + 'static,
) -> anyhow::Result<()> {
    let file = BufReader::new(File::open(path)?);
    let (tx, mut rx) = channels::channel();
    let handle = AppThreadHandle::from_sender(tx);
    DESERIALIZE_THREAD_HANDLE.with(|h| h.borrow_mut().replace(handle));
    let mut lines = file.lines();
    let loaded = LayoutEngine::deserialize_file(&lines.next().expect("Empty recording")?)?;
    let memory = loaded.memory;
    let layout = loaded.layout?;
    let (broadcast_tx, _) = channels::channel();
    let mut reactor = Reactor::new(
        config,
        layout,
        memory,
        Record::new(None),
        broadcast_tx,
        None,
        false,
    );
    std::thread::spawn(move || {
        while let Some((span, request)) = rx.blocking_recv() {
            let _ = span.enter();
            on_event(span, request);
        }
    });
    for line in lines {
        reactor.handle_event(ron::de::from_str(&line?)?);
    }
    Ok(())
}
