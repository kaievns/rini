//! The applications asking for attention, read from LaunchServices once a second on a thread of its
//! own and sent as `Event::AttentionChanged` whenever the set changes. No notification is read:
//! none is known to fire on the ask (`src/windows/docs/attention.md`).
//!
//! The calls are private to LaunchServices and resolved by `dlsym`, so rini still launches, and reads
//! no attention, on a macOS that lacks them.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::Duration;

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use once_cell::sync::Lazy;
use rini_core::ids::pid_t;
use tracing::warn;

use crate::windows::domain::attention::Attention;
use crate::windows::event::{Event, EventSink};
use crate::windows::platform::symbol;

/// `kLSDefaultSessionID`.
const SESSION: i32 = -2;
const EVERY: Duration = Duration::from_secs(1);
const PID: &str = "pid";
const WANTS_ATTENTION: &str = "LSWantsAttention";

/// Every running application's ASN. The second argument changed nothing in the values tried.
type CopyApplications = unsafe extern "C" fn(session: i32, unknown: u32) -> *const CFArray;
/// The keys named of the application an ASN names.
type CopyInformation = unsafe extern "C" fn(
    session: i32,
    asn: *const CFType,
    keys: *const CFArray,
) -> *const CFDictionary;

struct LaunchServices {
    applications: CopyApplications,
    information: CopyInformation,
}

static LAUNCH_SERVICES: Lazy<Option<LaunchServices>> = Lazy::new(|| {
    let applications = symbol(c"_LSCopyApplicationArrayInFrontToBackOrder")?;
    let information = symbol(c"_LSCopyApplicationInformation")?;
    // SAFETY: each symbol is the private function its type above declares, a signature known from
    // use, not from a header; no cast to a fn pointer is checked.
    Some(unsafe {
        LaunchServices {
            applications: std::mem::transmute::<*mut c_void, CopyApplications>(
                applications.as_ptr(),
            ),
            information: std::mem::transmute::<*mut c_void, CopyInformation>(information.as_ptr()),
        }
    })
});

/// Starts the reader. It sends nothing until an application asks.
pub fn spawn(sink: Box<dyn EventSink>) {
    let Some(launch_services) = LAUNCH_SERVICES.as_ref() else {
        warn!("LaunchServices has no application information calls; no window is drawn urgent");
        return;
    };
    std::thread::Builder::new()
        .name("attention".to_string())
        .spawn(move || {
            let mut sent = Attention::default();
            loop {
                let now = read(launch_services);
                if now != sent {
                    sink.send(Event::AttentionChanged(now.clone()));
                    sent = now;
                }
                std::thread::sleep(EVERY);
            }
        })
        .expect("failed to spawn the attention thread");
}

fn read(launch_services: &LaunchServices) -> Attention {
    let keys = CFArray::<CFString>::from_retained_objects(&[
        CFString::from_static_str(PID),
        CFString::from_static_str(WANTS_ATTENTION),
    ]);
    let keys: *const CFArray = CFRetained::as_ptr(&keys).as_ptr().cast();
    // SAFETY: integer arguments only; the array comes back +1 or null.
    let Some(applications) =
        NonNull::new(unsafe { (launch_services.applications)(SESSION, 0) }.cast_mut())
    else {
        return Attention::default();
    };
    // SAFETY: the call returns its array retained, and every element is an ASN, a CF object.
    let applications: CFRetained<CFArray<CFType>> =
        unsafe { CFRetained::from_raw(applications.cast()) };
    Attention::of(applications.iter().filter_map(|asn| {
        let asn: *const CFType = &*asn;
        // SAFETY: `asn` and `keys` are live CF objects, borrowed for the call.
        let answer =
            NonNull::new(unsafe { (launch_services.information)(SESSION, asn, keys) }.cast_mut())?;
        // SAFETY: returned retained, its keys the strings asked for.
        let answer: CFRetained<CFDictionary<CFString, CFType>> =
            unsafe { CFRetained::from_raw(answer.cast()) };
        asking(&answer)
    }))
}

/// The application an answer names, while it asks for attention: its `LSWantsAttention` is true,
/// where an application that never asked has none and one that stopped asking has it false.
fn asking(answer: &CFDictionary<CFString, CFType>) -> Option<pid_t> {
    let wants = answer
        .get(&CFString::from_static_str(WANTS_ATTENTION))?
        .downcast::<CFBoolean>()
        .ok()?;
    if !wants.as_bool() {
        return None;
    }
    answer
        .get(&CFString::from_static_str(PID))?
        .downcast::<CFNumber>()
        .ok()?
        .as_i32()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(pid: Option<i32>, wants: Option<bool>) -> CFRetained<CFDictionary<CFString, CFType>> {
        let pid = pid.map(CFNumber::new_i32);
        let mut keys = Vec::new();
        let mut values: Vec<&CFType> = Vec::new();
        let (pid_key, wants_key) = (
            CFString::from_static_str(PID),
            CFString::from_static_str(WANTS_ATTENTION),
        );
        if let Some(pid) = &pid {
            keys.push(&*pid_key);
            values.push(pid);
        }
        if let Some(wants) = wants {
            keys.push(&*wants_key);
            values.push(CFBoolean::new(wants));
        }
        CFDictionary::from_slices(&keys, &values)
    }

    #[test]
    fn an_application_asks_while_launch_services_says_it_wants_attention() {
        assert_eq!(asking(&answer(Some(42), Some(true))), Some(42));
        assert_eq!(asking(&answer(Some(42), Some(false))), None, "it stopped asking");
        assert_eq!(asking(&answer(Some(42), None)), None, "it never asked");
        assert_eq!(
            asking(&answer(None, Some(true))),
            None,
            "no application to draw urgent"
        );
    }
}
