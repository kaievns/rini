//! The focuslab tools' tests, run with the rest: a change to the journal's lines fails
//! `the_sample_the_tools_read_is_what_the_journal_writes` until the sample is written again to match,
//! and these say whether `tools/focuslab/journal.py` still reads it.

use std::process::Command;

#[test]
fn the_focuslab_tools_read_the_journal_as_it_is_written() {
    let ran = Command::new("python3")
        .args(["-B", "-m", "unittest", "discover", "-s", "tools/focuslab"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();
    let output = match ran {
        Ok(output) => output,
        Err(error) => {
            eprintln!("skipped: no python3 to run the focuslab tests ({error})");
            return;
        }
    };
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}
