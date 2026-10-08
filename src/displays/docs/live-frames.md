# Live frames: where the window server has a window

The window server tells rini when a window it tracks moves (806, `WindowMoved`) or resizes (807,
`WindowResized`). `cgs_notify::update_window_notifications` names every tracked window
(`SLSRequestNotificationsForWindows`), so the stream covers all of them, whoever moved the window.
A notification names the window and nothing else. `window_notify`'s `window-frames` thread reads the
frame back (`window_server::server_frame`, `SLSGetWindowBounds`) and hands the batch to every
`OnServerFrames` callback the application wires.

- **One reader.** SkyLight delivers each event type on its own channel, read on its own thread, so
  806 and 807 arrive unordered against each other. The two threads only forward the window id; the
  frames thread drains what has queued and reads each window once, after its last notification
  (`domain::frames::latest`). With two readers a frame read earlier could be published after a
  newer one.
- **The rest windows listen.** `main.rs` wires one callback, the decorations' rest windows
  (`src/decorations/platform/rest.rs`), which follow every decorated window the window server moves;
  they take the latest frame of each window from what queued for them on the main thread.
- **Only while followed.** The application turns `Following` (`displays::event`) on while any
  window can wear a band. Off, a move or a resize queues nothing for the frames thread, which reads
  no frame, and no reorder, hide, unhide or close reaches an `OnServerOrder` callback: each is
  dropped on the thread its notification arrives on (`note_move`, `publish_order`), and nothing
  reaches the main thread for them. The focus resolver still wakes on 808 and 815.

## Measured 2026-10-06

Kai pressed and dragged Ghostty windows 21 times while a probe logged the 806/807 stream with a
bounds read in the callback, accessibility's moved and resized notifications, the tap's presses,
and a 1ms `SLSGetWindowBounds` poll, on one clock. One row per press:
[`docs/data/2026-10-06-drags/presses.csv`](../../../docs/data/2026-10-06-drags/presses.csv). Delays
are against the first time the 1ms poll saw each frame; negative is ahead of it. A press is told
apart by the frames the poll saw while it was held: a left-edge drag moved the left edge with the
right one within 1pt of where it was, a right-edge drag changed the size otherwise, and a title-bar
move changed only the origin.

| Press | n | 806/807: frames seen | p50 | p90 | Accessibility: frames seen | reports/s | p50 | p90 |
|---|---|---|---|---|---|---|---|---|
| Right edge | 8 | 100% | -0.55 to -0.15ms | ≤0.51ms | 62-100% | 96-161 | 9.3-13.0ms ahead | 6.3-10.0ms ahead |
| Left edge | 9 | 100% | -0.42 to -0.05ms | ≤0.36ms | 56-98% | 80-168 | 8.9-13.5ms ahead | 8.2-11.2ms ahead |
| Title-bar move | 3 | 100% | -0.26 to -0.25ms | ≤0.14ms | 10-11% | 14-18 | 2.1-4.7ms late | 46-84ms late |

Press 4 was a click that moved nothing. The latest any 806/807 came after the poll was
6.25ms, once.

- **Accessibility runs ahead of an edge drag.** The app reports the size it is about to draw before
  the window server commits it, and misses up to 44% of the frames.
- **A title-bar move is the window server's alone.** The app hears of it late and rarely: about one
  frame in ten, the slowest 83-102ms after the screen. Each of the 3 moves ended in a swap.
- **The read is cheap but uneven.** The bounds read in the callback took p50 30-89µs per press,
  p90 0.12-2.5ms, at most 4.4ms.

So whatever follows a window at drag rate reads the window server, not accessibility. An edge drag
sends 48-166 notifications a second for the window in the hand.

Not measured: apps other than Ghostty, and the delay from the frames thread to the main thread while
a flight runs.
