//! What the Accessibility boundary's answers mean, without any Accessibility in it.
//!
//! Two rules the per-app thread used to carry inline, neither reachable from a test because the file
//! holding them needs a live `AXUIElement` to do anything at all.
//!
//! The first decides whether a failed request means the window is gone. Getting that wrong is not a
//! small matter: rini reacts to "gone" by destroying its record and telling the reactor, and
//! `docs/testing.md` records two tests that flapped because a negative answer about a window is not
//! neutral. The second packs a notification into the one `usize` an Accessibility observer callback
//! carries, which couples an enum's discriminants to a hand-written tag table.

use std::num::NonZeroU32;

use rini_core::ids::{WindowId, pid_t};

/// Why an Accessibility request failed, in rini's terms rather than macOS's.
///
/// The platform side translates its error codes into this; everything else reads this. That is what
/// keeps the rule below testable without an `AXUIElement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxFailure {
    /// The element itself is no longer valid. This is the ONLY answer that means the window is gone.
    ElementInvalid,
    /// The application did not answer. It is busy, or mid-launch, or wedged — but it is still there.
    AppBusy,
    /// Rini has no record of the window the request was about.
    Untracked,
    /// Anything else macOS reported.
    Other,
}

/// What to do about a failed Accessibility request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handling {
    /// Destroy rini's record of the window and tell the reactor.
    Retire,
    /// Treat it as no answer and carry on. The window stays registered.
    Ignore,
    /// Hand the error to the caller, which asked for something specific and needs to know.
    Propagate,
}

/// Whether a failure means the window is gone, or merely that this request did not work.
///
/// Only `ElementInvalid` retires a window. `AppBusy` explicitly does NOT: an application that is
/// slow to answer is the normal case during launch and under load, and retiring on it deletes live
/// windows. `Untracked` is nothing to do — there is no record to retire. Anything else goes back to
/// the caller rather than being swallowed, because a request nobody can explain should not look like
/// a success.
pub fn handling(failure: AxFailure) -> Handling {
    match failure {
        AxFailure::ElementInvalid => Handling::Retire,
        AxFailure::AppBusy | AxFailure::Untracked => Handling::Ignore,
        AxFailure::Other => Handling::Propagate,
    }
}

/// Whether a tracked window should be dropped on the strength of this failure alone.
///
/// Used by the periodic sweep, which asks every tracked window for its role and drops the ones whose
/// element has died. `kAXWindowsAttribute` is space-filtered and cannot answer whether a window
/// still exists globally, so absence from it proves nothing and only an invalid element does.
pub fn is_gone(failure: AxFailure) -> bool {
    matches!(handling(failure), Handling::Retire)
}

/// What an Accessibility observer notification is about.
///
/// The discriminants are the wire format: they are packed into the `usize` the observer callback
/// carries and read back by [`AxNotificationKind::from_tag`]. They start at 1 so that a zero tag,
/// which is what an uninitialised or absent value looks like, is not a valid kind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AxNotificationKind {
    ApplicationActivated = 1,
    ApplicationDeactivated,
    ApplicationHidden,
    ApplicationShown,
    MainWindowChanged,
    WindowCreated,
    MenuOpened,
    MenuClosed,
    WindowDestroyed,
    WindowMoved,
    WindowResized,
    WindowMiniaturized,
    WindowDeminiaturized,
    TitleChanged,
}

/// Every kind, in discriminant order. The round-trip test walks this, so a variant added without a
/// tag in `from_tag` fails rather than silently decoding as something else.
pub const ALL_NOTIFICATION_KINDS: [AxNotificationKind; 14] = [
    AxNotificationKind::ApplicationActivated,
    AxNotificationKind::ApplicationDeactivated,
    AxNotificationKind::ApplicationHidden,
    AxNotificationKind::ApplicationShown,
    AxNotificationKind::MainWindowChanged,
    AxNotificationKind::WindowCreated,
    AxNotificationKind::MenuOpened,
    AxNotificationKind::MenuClosed,
    AxNotificationKind::WindowDestroyed,
    AxNotificationKind::WindowMoved,
    AxNotificationKind::WindowResized,
    AxNotificationKind::WindowMiniaturized,
    AxNotificationKind::WindowDeminiaturized,
    AxNotificationKind::TitleChanged,
];

impl AxNotificationKind {
    pub fn from_tag(tag: u8) -> Option<Self> {
        ALL_NOTIFICATION_KINDS.into_iter().find(|kind| *kind as u8 == tag)
    }
}

/// How many low bits of the packed value the kind tag occupies.
const KIND_BITS: usize = 8;
const KIND_MASK: usize = (1 << KIND_BITS) - 1;

/// Pack a notification and the window it is about into the one `usize` an observer callback carries.
///
/// The window index goes above the kind tag. A notification about the application rather than a
/// window has no index, and packs as zero — which is why `WindowId`'s index is a `NonZeroU32`: zero
/// is how "no window" is spelled, and a window with index 0 would be indistinguishable from it.
pub fn encode_notification_data(kind: AxNotificationKind, wid: Option<WindowId>) -> usize {
    let idx = wid.map_or(0, |wid| wid.idx.get()) as usize;
    (idx << KIND_BITS) | kind as usize
}

/// Read back what [`encode_notification_data`] packed. `None` means the tag is not a kind rini knows.
pub fn decode_notification_data(
    pid: pid_t,
    data: usize,
) -> Option<(AxNotificationKind, Option<WindowId>)> {
    let kind = AxNotificationKind::from_tag((data & KIND_MASK) as u8)?;
    let idx = NonZeroU32::new((data >> KIND_BITS) as u32);
    Some((kind, idx.map(|idx| WindowId { pid, idx })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule the two flapping tests in `docs/testing.md` were about. An application that is slow
    /// to answer is the normal case during launch and under load; retiring on it deletes live
    /// windows.
    #[test]
    fn only_an_invalid_element_retires_a_window() {
        assert_eq!(handling(AxFailure::ElementInvalid), Handling::Retire);
        assert_eq!(handling(AxFailure::AppBusy), Handling::Ignore);
        assert_eq!(handling(AxFailure::Untracked), Handling::Ignore);
    }

    /// An error nobody can explain goes back to the caller rather than looking like a success.
    #[test]
    fn an_unexplained_failure_is_not_swallowed() {
        assert_eq!(handling(AxFailure::Other), Handling::Propagate);
    }

    #[test]
    fn the_sweep_drops_a_window_only_when_its_element_is_invalid() {
        assert!(is_gone(AxFailure::ElementInvalid));
        for failure in [AxFailure::AppBusy, AxFailure::Untracked, AxFailure::Other] {
            assert!(!is_gone(failure), "{failure:?} is not proof a window is gone");
        }
    }

    /// The coupling this exists to pin: the enum's discriminants ARE the wire format, and `from_tag`
    /// has to agree with them. A variant added at the wrong position decodes as its neighbour.
    #[test]
    fn every_kind_round_trips_through_its_tag() {
        for kind in ALL_NOTIFICATION_KINDS {
            assert_eq!(AxNotificationKind::from_tag(kind as u8), Some(kind), "{kind:?}");
        }
    }

    #[test]
    fn every_kind_has_a_distinct_tag() {
        let mut tags: Vec<u8> = ALL_NOTIFICATION_KINDS.iter().map(|k| *k as u8).collect();
        let before = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), before, "two kinds share a tag");
    }

    /// Zero is how "no window" is spelled in the packed value, so it must not also be a valid kind.
    #[test]
    fn zero_is_not_a_kind() {
        assert_eq!(AxNotificationKind::from_tag(0), None);
    }

    #[test]
    fn a_tag_past_the_last_kind_is_not_a_kind() {
        assert_eq!(
            AxNotificationKind::from_tag(ALL_NOTIFICATION_KINDS.len() as u8 + 1),
            None
        );
        assert_eq!(AxNotificationKind::from_tag(u8::MAX), None);
    }

    /// Every kind has to fit in the low bits, or it would overwrite the window index.
    #[test]
    fn every_kind_fits_in_the_bits_reserved_for_it() {
        for kind in ALL_NOTIFICATION_KINDS {
            assert!(
                (kind as usize) <= KIND_MASK,
                "{kind:?} does not fit in {KIND_BITS} bits; the packing needs widening"
            );
        }
    }

    #[test]
    fn a_notification_about_a_window_round_trips() {
        let pid: pid_t = 501;
        let wid = WindowId::new(pid, 42);
        for kind in ALL_NOTIFICATION_KINDS {
            let data = encode_notification_data(kind, Some(wid));
            assert_eq!(
                decode_notification_data(pid, data),
                Some((kind, Some(wid))),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_notification_about_the_application_carries_no_window() {
        let data = encode_notification_data(AxNotificationKind::ApplicationActivated, None);
        assert_eq!(
            decode_notification_data(501, data),
            Some((AxNotificationKind::ApplicationActivated, None))
        );
    }

    /// The index and the tag must not bleed into each other. A large index is the case that catches
    /// a shift written with the wrong width.
    #[test]
    fn a_large_window_index_does_not_disturb_the_kind() {
        let pid: pid_t = 7;
        let wid = WindowId::new(pid, u32::MAX);
        let data = encode_notification_data(AxNotificationKind::TitleChanged, Some(wid));
        assert_eq!(
            decode_notification_data(pid, data),
            Some((AxNotificationKind::TitleChanged, Some(wid)))
        );
    }

    /// The decoded window belongs to the process that decoded it. The packed value carries only an
    /// index, so the pid comes from the thread reading it, which is the only thing that knows.
    #[test]
    fn the_decoded_window_belongs_to_the_decoding_process() {
        let data =
            encode_notification_data(AxNotificationKind::WindowMoved, Some(WindowId::new(1, 9)));
        let (_, wid) = decode_notification_data(999, data).unwrap();
        assert_eq!(wid, Some(WindowId::new(999, 9)));
    }
}
