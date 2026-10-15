#![allow(non_camel_case_types, reason = "SkyLight's C names")]
//! The SkyLight connection-notification port: one registration per CGS event type, fanned out to
//! a channel per type, plus the per-window filter SkyLight needs for window-level events.

// based on https://github.com/koekeishiya/yabai/commit/6f9006dd957100ec13096d187a8865e85a164a9b#r148091577
// seems like macOS Sequoia does not send destroyed events from windows that are before the process is created

// https://github.com/asmagill/hs._asm.undocumented.spaces/blob/0b5321fc336f75488fb4bbb524677bb8291050bd/CGSConnection.h#L153
// https://github.com/NUIKit/CGSInternal/blob/c4f6f559d624dc1cfc2bf24c8c19dbf653317fcf/CGSEvent.h#L21

use std::ffi::c_void;

use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};
use rini_runloop::channel as channels;
use rini_skylight_sys::{
    KnownCGSEvent, SLSMainConnectionID, SLSRegisterConnectionNotifyProc,
    SLSRequestNotificationsForWindows, cid_t,
};
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, trace, warn};

type Wid = u32;
type Sid = u64;

#[derive(Debug, Clone)]
pub struct EventData {
    pub window_id: Option<Wid>,
    pub space_id: Option<Sid>,
}

static EVENT_RECEIVERS: Lazy<Mutex<HashMap<KnownCGSEvent, Option<channels::Receiver<EventData>>>>> =
    Lazy::new(|| Mutex::new(HashMap::default()));

static G_CONNECTION: Lazy<cid_t> = Lazy::new(|| unsafe { SLSMainConnectionID() });

static CALLBACK_CTXS: Lazy<RwLock<HashMap<KnownCGSEvent, Box<CallbackContext>>>> =
    Lazy::new(|| RwLock::new(HashMap::default()));

struct CallbackContext {
    event_type: KnownCGSEvent,
    sender: channels::Sender<EventData>,
}

/// Registers `event` with SkyLight. MUST be called at most once per event type for the process: a
/// second call registers the callback again over the first call's context, so the first receiver
/// gets every event twice and the second receiver none.
pub fn init(event: KnownCGSEvent) -> i32 {
    let mut callback_ctxs = CALLBACK_CTXS.write();
    debug_assert!(
        !callback_ctxs.contains_key(&event),
        "cgs_notify::init({event}) called twice"
    );

    let (tx, rx) = channels::channel::<EventData>();
    EVENT_RECEIVERS.lock().insert(event, Some(rx));

    let callback_ctx = callback_ctxs
        .entry(event)
        .or_insert_with(|| Box::new(CallbackContext { event_type: event, sender: tx }));

    let raw = event as u32;
    let callback_ctx_ptr = (&mut **callback_ctx as *mut CallbackContext).cast::<c_void>();
    let res = unsafe {
        SLSRegisterConnectionNotifyProc(*G_CONNECTION, connection_callback, raw, callback_ctx_ptr)
    };

    if res == 0 {
        debug!("registered {} (raw={}) callback, res={}", event, raw, res);
    } else {
        warn!("failed to register event {} (raw={}), res={}", event, raw, res);
    }

    res
}

pub fn take_receiver(event: KnownCGSEvent) -> channels::Receiver<EventData> {
    if let Some(rx) = EVENT_RECEIVERS.lock().get_mut(&event)
        && let Some(rxo) = rx.take()
    {
        rxo
    } else {
        panic!("window_notify::take_receiver({}) failed", event)
    }
}

pub fn update_window_notifications(window_ids: &[u32]) {
    unsafe {
        let _ = SLSRequestNotificationsForWindows(
            *G_CONNECTION,
            window_ids.as_ptr(),
            window_ids.len() as i32,
        );
    }
}

#[inline(always)]
fn read<T: Copy + Sized + 'static>(bytes: &[u8], off: usize) -> Option<T> {
    let n = std::mem::size_of::<T>();
    if bytes.len() < off + n {
        return None;
    }
    let mut buf = [0u8; 32];
    assert!(n <= buf.len(), "read_ne: type too large");
    buf[..n].copy_from_slice(&bytes[off..off + n]);
    Some(unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const T) })
}

extern "C" fn connection_callback(
    event_raw: u32,
    data: *mut c_void,
    len: usize,
    context: *mut c_void,
    _cid: cid_t,
) {
    if context.is_null() {
        return;
    }

    let ctx = unsafe { &*(context as *const CallbackContext) };
    let kind = ctx.event_type;
    debug_assert_eq!(event_raw, kind as u32);
    let sender = ctx.sender.clone();

    let bytes = if data.is_null() || len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data as *const u8, len) }
    };

    let mut window_id = None;
    let mut space_id = None;

    match kind {
        KnownCGSEvent::SpaceWindowDestroyed | KnownCGSEvent::SpaceWindowCreated => {
            if let Some(sid) = read::<u64>(bytes, 0) {
                if let Some(wid) = read::<u32>(bytes, std::mem::size_of::<u64>()) {
                    space_id = Some(sid);
                    window_id = Some(wid);
                } else {
                    warn!("Skylight event {kind} payload too short for window id (len={len})");
                }
            } else {
                warn!("Skylight event {kind} payload too short for space id (len={len})");
            }
        }

        KnownCGSEvent::SpaceCreated | KnownCGSEvent::SpaceDestroyed => {
            if let Some(sid) = read::<u64>(bytes, 0) {
                space_id = Some(sid)
            } else {
                warn!("no space_id on {kind}");
            }
        }

        KnownCGSEvent::WindowClosed
        | KnownCGSEvent::WindowMoved
        | KnownCGSEvent::WindowResized
        | KnownCGSEvent::WindowReordered
        | KnownCGSEvent::WindowUnhidden
        | KnownCGSEvent::WindowHidden => {
            if let Some(wid) = read::<u32>(bytes, 0) {
                window_id = Some(wid);
            } else {
                warn!("Skylight event {kind} payload too short for window id (len={len})");
            }
        }

        _ => {}
    }

    let event_data = EventData { window_id, space_id };

    trace!("received raw event {kind} (len={len}): {:?}", event_data);

    if let Err(e) = sender.try_send(event_data) {
        debug!("Failed to send event {kind}: {e}");
    }
}
