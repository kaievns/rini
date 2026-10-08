# Window-server reads and the connection they go on

Every read of windows that waits on its connection's Core Animation transactions is made on
`READ_CONNECTION` (`src/windows/platform/window_server.rs`), a connection rini opens with
`SLSNewConnection` and that no window of rini's belongs to. Writes to rini's own windows, and the
reads that wait on nothing, stay on the main connection (`SLSMainConnectionID`). If
`SLSNewConnection` fails, rini logs a warning and reads on the main connection.
`a_read_that_waits_on_core_animation_is_made_on_the_read_connection` in `tests/architecture.rs`
fails if a call that waits is passed anything but `*READ_CONNECTION`.

Data, sources and how to run them again:
[`docs/data/2026-10-08-skylight-lock/`](../../../docs/data/2026-10-08-skylight-lock/).

## The lock, measured 2026-10-08

Kai: "all the animations are _significantly_ slower with the rings". A 3s
`sample` of rini (pid 77575, 4cddd692) held a 500ms deadlock between three threads
(`stall-stack.txt`; 2291 main-thread samples of about 1.31ms each):

| thread | samples | where |
|---|---|---|
| main | 403, about 528ms | the run loop's implicit Core Animation flush: `sls_transaction_commit_ca` → `SLSConnectionSetLastSLSCATransaction`, waiting for the main connection's lock |
| reactor | 405 | `Reactor::apply_overlay_frames` → `get_windows` → `SLSWindowQueryWindows` → `run_query` → `SLSConnectionSynchronizeSLSCATransaction` → `_SLSTransactionWaitSource`, holding the lock while it waits for the connection's last CA-entangled transaction |
| transitions | 126 | `FlightEngine::handover` → `get_windows` → the same `SLSConnectionSynchronizeSLSCATransaction`, queued on the lock |

`SLSConnectionSynchronizeSLSCATransaction` takes the connection's `os_unfair_lock`, calls
`SLSTransactionWait` and unlocks only after it (`callgraph.txt`'s disassembly). The transaction it
waits for is the one the main thread is committing, and that commit needs the same lock to record
itself, so neither moves until the wait times out at 500ms. In the repro below a frame change of a
window of the reading process is what leaves such a transaction pending; in rini that is the rest
windows' `setFrame:display:` (`src/decorations/platform/rest.rs`), which the ring and the border
make whenever a window they follow moves, inferred from the code rather than seen in the sample.

## The repro

`repro.m` moves a borderless layer-backed panel every 4ms on the main thread while a second thread
reads windows in a loop. A query over 50ms counts as slow; each run's worst is in the last columns.

| reads on | runs | slow queries | worst query per run | worst main-thread turn per run | file |
|---|---|---|---|---|---|
| `SLSMainConnectionID`, `SLSWindowQueryWindows` | 6 | 23 | 500.0-503.6ms | 500.3-506.2ms | `ab.txt` |
| an `SLSNewConnection`, `SLSWindowQueryWindows` | 6 | 0 | 15.2-29.1ms | 57.2-76.4ms | `ab.txt` |
| `CGWindowListCopyWindowInfo`, no connection to choose | 4 of 10s | 41 | 501.7-504.3ms | 500.8-504.1ms | `cg.txt` |

`ops.txt` splits the panel's changes, 2 runs each on the main connection: alpha alone and order
alone gave no slow query; the frame alone gave 5 in one run of its 2.

## Which reads wait

`sls-graph.py` disassembles each SkyLight call rini makes and its callees 4 deep, and reports a path
to `SLSConnectionSynchronizeSLSCATransaction`, `SLSTransactionWait` or `run_query`
(`callgraph.txt`). The iterator getters read the reply already in hand and wait on nothing.

**On `READ_CONNECTION`**, each because it waits:

| call | path to the wait | rini's reads through it |
|---|---|---|
| `SLSWindowQueryWindows` | `run_query` | `WindowIterator`: `get_windows`, `get_window`, `existing_windows`, `window_parent`, `window_level` (so `StackPlace::of`), `corner_radii`, `app_window_suitability`, `space_window_list_for_connection`'s details |
| `SLSWindowQueryRun` | `run_query` | `key_focused_window` |
| `SLSGetWindowBounds` | `GetWindowBounds` → `CGSWindowSynchronizeSLSCATransaction` | `server_frame`: the `window-frames` thread (`src/displays/docs/live-frames.md`) and the pictures watches |
| `SLSWindowIsOrderedIn` | direct | `window_ordered_in` |
| `SLSCopyWindowsWithOptionsAndTags` | direct | `space_window_list_for_connection` |
| `SLSHWCaptureWindowList` | `hw_capture_window_list_common` | every SkyLight composite in `src/pictures/platform/skylight.rs` |
| `SLSCaptureWindowsContentsToRectWithOptions` | direct | the bar's menu-extras capture |

**On the main connection**, each for its reason:

| call | why it stays |
|---|---|
| `SLSSetWindowLevel`, `SLSSetWindowSubLevel` | writes to rini's own windows, which belong to the main connection; the window server levels only a connection's own windows |
| `SLSFindWindowAndOwner` | waits on nothing; the owner it answers is compared to the main connection to step past rini's own windows |
| `SLSRegisterConnectionNotifyProc`, `SLSRequestNotificationsForWindows` | the notifications belong to the connection that asks for them, and rini's arrive on its main one |
| `CGSSetConnectionProperty` (`SetsCursorInBackground`) | a property of the connection whose cursor rini hides |
| `SLSPackagesGetWindowConstraints`, `SLSGetCurrentCursorLocation`, `SLSGetConnectionIDForPSN`, `SLPSGetKeyFocusProcess`, `SLSGetMenuBarAutohideEnabled`, `SLSGetDockRectWithReason`, `SLSGetDisplayMenubarHeight` | wait on nothing: a port message or the process's own state |
| `SLSCopySpacesForWindows`, `SLSSpaceGetType`, `SLSGetSpaceManagementMode`, `SLSManagedDisplaysCopyRoleWindows`, `SLSCopyActiveMenuBarDisplayIdentifier`, and the `CGS*` spaces and managed-display calls, checked under their `SLS` names | wait on nothing: the window-management bridge or a port message |

The sub-level read (`src/windows/platform/sub_level.rs`) is a Mach message to a connection server
port of its own, and takes no connection's lock.

## Not yet met: the reads with no connection to choose

`CGWindowListCopyWindowInfo` and `CGWindowListCreateImage` call `SLSMainConnection` and then
`SLSConnectionSynchronizeSLSCATransaction` (`callgraph.txt`, as `SLWindowListCopyWindowInfo` and
`SLWindowListCreateImage`), and take no connection. `cg.txt` shows them stall 500ms the same way.
Through them rini still reads on the main connection:

- `front_to_back_depths`: the `transitions` thread at the start of every flight, and the reactor's
  strip regroup;
- `visible_windows_on_display`: the pictures warm and the debug slide;
- `desktop_backdrop_windows`, `bar_strip` and the menu-extras listing;
- the framed spawn capture on the `transitions` thread (`capture_via_framed`) and the handover
  measure's `capture_screen`.

## The read connection answers as the main one does

Measured 2026-10-08 on 23 on-screen layer-0 windows of 11 processes, 4 of them rini's:

- `same.txt`: `SLSWindowQueryWindows` returned the same id, pid, level, alpha and bounds on both.
- `reads.txt`: `SLSGetWindowBounds`, `SLSWindowIsOrderedIn`, `SLSPackagesGetWindowConstraints`, the
  iterator's corner radii, and both captures' size and pixels agreed for 21. The other 2 changed
  content between asks: the main connection, asked again after the new one, differed from its own
  first answer too, so `reads.m` counts them as live. `SLSCopyWindowsWithOptionsAndTags` listed the
  same 45 windows of the active space in the same order.
- `capcost.txt`: a capture costs the same on either connection. Interleaved, p50 over 21 each, at
  gaps of 0, 20, 120 and 300ms since the previous capture: 17.2/17.0, 24.1/22.8, 43.5/40.6 and
  45.5/49.0ms, main/new.

Not measured: rini itself on the read connection, sampled through a flight with the ring on.
