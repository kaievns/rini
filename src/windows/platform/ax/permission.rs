//! The Accessibility permission: prompt for it, wait for it, and say when it is granted.
use std::ffi::c_void;
use std::thread;
use std::time::{Duration, Instant};

use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use tracing::info;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;

    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFBooleanFalse: *const c_void;
}

/// Whether to let macOS put the permission dialog up while asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prompt {
    /// Ask and show the dialog. Interrupts the user, so it is used once.
    Yes,
    /// Ask quietly, which is what the poll below does every 250ms.
    No,
}

const AX_POLL_INTERVAL: Duration = Duration::from_millis(250);
const AX_POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether this process is trusted for Accessibility, optionally showing the system dialog.
///
/// One call for both questions because `AXIsProcessTrustedWithOptions` answers both: the prompt
/// option is what decides whether macOS puts the permission dialog up, and the return value is the
/// answer either way. The two were written out separately, differing only in
/// `kCFBooleanFalse` versus `kCFBooleanTrue` — which is precisely the difference between asking
/// quietly and interrupting the user, and worth a named argument rather than a copied body.
///
/// macOS shows the dialog at most once per process; later prompting calls return without one.
fn ax_trusted(prompt: Prompt) -> bool {
    unsafe {
        let prompt_value = match prompt {
            Prompt::Yes => kCFBooleanTrue,
            Prompt::No => kCFBooleanFalse,
        };
        autoreleasepool(|_| {
            let keys: [*mut AnyObject; 1] = [kAXTrustedCheckOptionPrompt as *mut AnyObject];
            let vals: [*mut AnyObject; 1] = [prompt_value as *mut AnyObject];
            let dict: *mut AnyObject = msg_send![
                class!(NSDictionary),
                dictionaryWithObjects: vals.as_ptr(),
                forKeys:              keys.as_ptr(),
                count:                1usize
            ];

            AXIsProcessTrustedWithOptions(dict.cast())
        })
    }
}

pub fn ensure_accessibility_permission() {
    if ax_trusted(Prompt::No) {
        return;
    }

    info!("Accessibility permission is not granted; prompting user for permission now.");

    ax_trusted(Prompt::Yes);

    let start = Instant::now();
    loop {
        if ax_trusted(Prompt::No) {
            info!("Accessibility permission granted");
            return;
        }

        if start.elapsed() >= AX_POLL_TIMEOUT {
            break;
        }

        thread::sleep(AX_POLL_INTERVAL);
    }

    println!(
        "Rini still does not have accessibility permission. Enable it in System Settings > Privacy & Security > Accessibility, then restart Rini."
    );

    std::process::exit(1);
}
