//! The journal's writer: the `journal` thread, which appends each burst the engine sends it as JSON
//! lines, its marks' instants on the uptime clock. Formatting and IO happen here, never on the main
//! thread. See `src/transitions/docs/journal.md`.

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tracing::warn;

use crate::transitions::domain::journal::{Anchor, Burst, lines};

/// The file is started afresh past this, the one before kept beside it.
const ROTATE_PAST: u64 = 50 * 1024 * 1024;

/// The engine's end of the `journal` thread. Dropped, the thread ends once it has written what it was
/// sent.
pub struct Writer {
    bursts: mpsc::Sender<Burst>,
}

impl Writer {
    /// The thread, appending to `path`; `None`, logged, when it cannot be spawned.
    pub fn start(path: PathBuf) -> Option<Writer> {
        let (bursts, sent) = mpsc::channel::<Burst>();
        let spawned = thread::Builder::new().name("journal".to_owned()).spawn(move || {
            for burst in sent {
                let anchor = Anchor { at: Instant::now(), uptime_ns: uptime_ns() };
                if let Err(error) = append(&path, ROTATE_PAST, &anchor, unix_ms(), &burst) {
                    warn!(%error, path = %path.display(), "could not write the transitions journal");
                }
            }
        });
        match spawned {
            Ok(_) => Some(Writer { bursts }),
            Err(error) => {
                warn!(%error, "could not start the transitions journal's thread");
                None
            }
        }
    }

    /// A writer with no thread behind it, whose bursts the caller reads.
    #[cfg(test)]
    pub fn detached() -> (Writer, mpsc::Receiver<Burst>) {
        let (bursts, sent) = mpsc::channel::<Burst>();
        (Writer { bursts }, sent)
    }

    pub fn send(&self, burst: Burst) {
        _ = self.bursts.send(burst);
    }
}

/// The burst's lines, appended to `path` once the file before them is moved aside if it is past
/// `limit`; its header says the wall clock.
fn append(path: &Path, limit: u64, anchor: &Anchor, unix_ms: u64, burst: &Burst) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::metadata(path).is_ok_and(|file| file.len() > limit) {
        fs::rename(path, path.with_extension("1.jsonl"))?;
    }
    let mut file = BufWriter::new(OpenOptions::new().create(true).append(true).open(path)?);
    for text in lines(anchor, unix_ms, burst) {
        writeln!(file, "{text}")?;
    }
    file.flush()
}

/// Nanoseconds on the clock `Instant` reads and the outside probe's `DispatchTime.uptimeNanoseconds`
/// does: mach absolute time, which stops while the machine sleeps.
pub fn uptime_ns() -> u64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `now` is a valid timespec to write into, and CLOCK_UPTIME_RAW exists on every macOS
    // rini runs on.
    unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut now) };
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::transitions::domain::journal::{Mark, Stamped};
    use crate::transitions::domain::lifecycle::Generation;

    fn burst(marks: usize, lost: u64) -> Burst {
        let at = Instant::now();
        Burst { marks: vec![Stamped { at, generation: Generation(7), mark: Mark::Hold }; marks], lost }
    }

    fn lines(path: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(path).unwrap().lines().map(|text| serde_json::from_str(text).unwrap()).collect()
    }

    #[test]
    fn a_burst_appends_a_header_and_its_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rini").join("transitions.jsonl");
        let anchor = Anchor { at: Instant::now(), uptime_ns: 1_000 };
        append(&path, ROTATE_PAST, &anchor, 42, &burst(2, 0)).unwrap();
        append(&path, ROTATE_PAST, &anchor, 43, &burst(1, 5)).unwrap();
        let written = lines(&path);
        let marks: Vec<&str> = written.iter().map(|line| line["mark"].as_str().unwrap()).collect();
        assert_eq!(marks, ["burst", "hold", "hold", "burst", "hold"], "appended, the directory made");
        assert_eq!(written[3]["unix_ms"], 43);
        assert_eq!(written[3]["lost"], 5);
        assert_eq!(written[1]["g"], 7);
    }

    #[test]
    fn the_file_rotates_past_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transitions.jsonl");
        let anchor = Anchor { at: Instant::now(), uptime_ns: 1_000 };
        append(&path, ROTATE_PAST, &anchor, 1, &burst(1, 0)).unwrap();
        let limit = fs::metadata(&path).unwrap().len();
        append(&path, limit, &anchor, 2, &burst(1, 0)).unwrap();
        assert!(!dir.path().join("transitions.1.jsonl").exists(), "not past it yet");
        append(&path, limit, &anchor, 3, &burst(1, 0)).unwrap();
        let before = lines(&dir.path().join("transitions.1.jsonl"));
        assert_eq!(before.len(), 4, "the two bursts before it, kept beside");
        assert_eq!(lines(&path).len(), 2, "started afresh");
        assert_eq!(lines(&path)[0]["unix_ms"], 3);
        append(&path, 0, &anchor, 4, &burst(1, 0)).unwrap();
        assert_eq!(lines(&dir.path().join("transitions.1.jsonl"))[0]["unix_ms"], 3, "only the one before is kept");
    }

    /// `Instant`'s `Debug` is the clock it reads, `{ tv_sec, tv_nsec }`; it must be `uptime_ns`'s.
    /// Equal deltas alone would pass for `CLOCK_MONOTONIC`, which differs only across a sleep.
    #[test]
    fn instant_reads_the_probe_clock() {
        let instant = Instant::now();
        let uptime = uptime_ns();
        let debug = format!("{instant:?}");
        let field = |name: &str| -> u64 {
            let rest = &debug[debug.find(name).unwrap_or_else(|| panic!("{debug}")) + name.len()..];
            rest.trim_start_matches([':', ' ']).split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
        };
        let read = field("tv_sec") * 1_000_000_000 + field("tv_nsec");
        assert!(uptime >= read && uptime - read < Duration::from_millis(1).as_nanos() as u64, "{debug} against {uptime}");
    }
}
