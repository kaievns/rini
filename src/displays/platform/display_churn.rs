//! Whether a display reconfiguration is under way, as the spaces actor alone writes it and other
//! threads read it the moment it changes. The reactor reads it rather than wait for
//! `DisplayChurnBegin`, which reaches it behind the events already on its queue.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The one writer, which the spaces actor holds, and a reader of the same record.
pub fn shared() -> (ChurnWriter, ChurnReader) {
    let churning = Arc::new(AtomicBool::new(false));
    (ChurnWriter(churning.clone()), ChurnReader(churning))
}

pub struct ChurnWriter(Arc<AtomicBool>);

impl ChurnWriter {
    pub fn begin(&self) { self.0.store(true, Ordering::SeqCst); }

    pub fn end(&self) { self.0.store(false, Ordering::SeqCst); }
}

#[derive(Clone, Debug)]
pub struct ChurnReader(Arc<AtomicBool>);

impl ChurnReader {
    /// A reader no writer moves: never churning.
    pub fn steady() -> Self { shared().1 }

    pub fn churning(&self) -> bool { self.0.load(Ordering::SeqCst) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_sees_its_writer_begin_and_end() {
        let (writer, reader) = shared();
        assert!(!reader.churning());
        writer.begin();
        assert!(reader.churning());
        assert!(reader.clone().churning());
        writer.end();
        assert!(!reader.churning());
    }

    #[test]
    fn two_records_never_see_each_other() {
        let (writer, _) = shared();
        let (_, other) = shared();
        writer.begin();
        assert!(!other.churning());
        assert!(!ChurnReader::steady().churning());
    }
}
