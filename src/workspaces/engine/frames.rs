//! The frames a layout pass writes for one space: the strip's columns, the floating layer, the
//! floating windows maximized over the tiling area, and every window of a workspace not showing,
//! parked in a corner. A pass spends what the strip held; a preview, for a query, leaves it.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::{SpaceId, WindowId};
use rustc_hash::FxHashMap as HashMap;

use crate::layout::settings::Gaps;
use crate::workspaces::engine::{LayoutEngine, LayoutId};
use crate::workspaces::{HiddenWindowPlacement, HideCorner, VirtualWorkspaceId, WindowStore};

/// The size a window is taken to have when its frame cannot be read.
const UNKNOWN_SIZE: CGSize = CGSize::new(500.0, 500.0);

/// One layout pass: the space and screen it lays out, every screen attached, and the frames so far.
struct FramePass<'a> {
    space: SpaceId,
    screen: CGRect,
    all_screens: &'a [CGRect],
    frames: HashMap<WindowId, CGRect>,
}

/// How a floating window's settled frame is saved as its frame in its workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveFrame {
    /// Over whatever the workspace saved for it: it is showing, and this is where it is.
    Replace,
    /// Only when the workspace has none for it: it is being parked, and the frame it comes back to
    /// is one it already has.
    IfAbsent,
}

/// One floating window to settle in a pass: its workspace, the frame it is offered, and how that is
/// saved.
struct Settle {
    workspace: VirtualWorkspaceId,
    window: WindowId,
    candidate: Option<CGRect>,
    save: SaveFrame,
}

fn size_or_unknown(frame: Option<CGRect>) -> CGSize { frame.map_or(UNKNOWN_SIZE, |f| f.size) }

/// Where a strip column's frame is written. A column scrolled off the strip is parked in the
/// bottom corner on the side it went off rather than written at its strip coordinate: macOS
/// refuses the coordinate and leaves 40pt of the window showing at the edge, and honours the
/// corner at 1pt. See "macOS will not park a window further off the left edge than 40pt" in
/// `src/animation/docs/capture-overlay-research.md`.
fn strip_frame(screen: CGRect, rect: CGRect, all_screens: &[CGRect]) -> CGRect {
    if !HiddenWindowPlacement::is_off_screen(screen, rect) {
        return rect;
    }
    let corner = if rect.max().x <= screen.origin.x {
        HideCorner::BottomLeft
    } else {
        HideCorner::BottomRight
    };
    HiddenWindowPlacement::calculate_among(screen, rect, corner, all_screens)
}

/// `candidate`, when it is not hidden among `all_screens` and enough of it is left on `screen` to
/// grab. The hidden test alone does not catch a park: it needs a sliver in BOTH axes, and a park
/// shows nearly its full height.
fn usable_floating_frame(
    screen: CGRect,
    all_screens: &[CGRect],
    candidate: Option<CGRect>,
) -> Option<CGRect> {
    candidate.filter(|rect| {
        !HiddenWindowPlacement::is_hidden_among(screen, *rect, all_screens)
            && HiddenWindowPlacement::floating_frame_is_usable(screen, *rect)
    })
}

fn centred_on(screen: CGRect, size: CGSize) -> CGRect {
    let center = screen.mid();
    let origin = CGPoint::new(center.x - size.width / 2.0, center.y - size.height / 2.0);
    CGRect::new(origin, size)
}

/// The tiling area inside `screen`'s outer gaps, which a maximized floating window fills.
fn maximized_frame(screen: CGRect, gaps: &Gaps) -> CGRect {
    let o = &gaps.outer;
    CGRect::new(
        CGPoint::new(screen.origin.x + o.left, screen.origin.y + o.top),
        CGSize::new(
            screen.size.width - o.left - o.right,
            screen.size.height - o.top - o.bottom,
        ),
    )
}

/// Where a window of a workspace not showing is parked: the bottom-right corner, from its own
/// frame, or from an unknown-sized window at the screen's origin when its frame cannot be read.
fn parked_frame(screen: CGRect, frame: Option<CGRect>, all_screens: &[CGRect]) -> CGRect {
    let reference = frame.unwrap_or_else(|| CGRect::new(screen.origin, UNKNOWN_SIZE));
    HiddenWindowPlacement::calculate_among(screen, reference, HideCorner::BottomRight, all_screens)
}

impl LayoutEngine {
    pub fn calculate_layout_with_virtual_workspaces<F>(
        &mut self,
        window_store: &WindowStore,
        space: SpaceId,
        screen: CGRect,
        gaps: &Gaps,
        get_window_frame: F,
        all_screens: &[CGRect],
    ) -> Vec<(WindowId, CGRect)>
    where
        F: Fn(WindowId) -> Option<CGRect>,
    {
        let window_size = |wid| size_or_unknown(get_window_frame(wid));
        let mut pass = FramePass {
            space,
            screen,
            all_screens,
            frames: HashMap::default(),
        };
        if let Some(workspace) = self.virtual_workspace_manager.active_workspace(space) {
            self.strip_frames(&mut pass, workspace, gaps);
            self.floating_frames(window_store, &mut pass, workspace, &window_size);
            let maximized: Vec<WindowId> =
                pass.frames.keys().copied().filter(|w| self.floating.is_maximized(*w)).collect();
            for window in maximized {
                pass.frames.insert(window, maximized_frame(screen, gaps));
            }
        }
        self.parked_frames(window_store, &mut pass, &get_window_frame, &window_size);
        pass.frames.into_iter().collect()
    }

    /// The strip's columns, as `strip_frame` writes them. `calculate_layout_for_workspace` answers
    /// with the real strip positions, which is what the animation canvas is built from.
    fn strip_frames(&mut self, pass: &mut FramePass, workspace: VirtualWorkspaceId, gaps: &Gaps) {
        let Some(layout) = self.workspace_layouts.active(pass.space, workspace) else {
            return;
        };
        let columns = self.lay_out_strip(workspace, layout, pass.screen, gaps);
        for (wid, rect) in columns {
            pass.frames.insert(wid, strip_frame(pass.screen, rect, pass.all_screens));
        }
    }

    /// The showing workspace's floating windows: each at its saved frame, then each in the
    /// floating layer, kept where it is when that is usable and centred when not.
    fn floating_frames(
        &mut self,
        window_store: &WindowStore,
        pass: &mut FramePass,
        workspace: VirtualWorkspaceId,
        window_size: &impl Fn(WindowId) -> CGSize,
    ) {
        for (window, saved) in self.floating_positions.workspace_positions(pass.space, workspace) {
            if self.floating.is_floating(window)
                && self.virtual_workspace_manager.workspace_for_window(
                    window_store,
                    pass.space,
                    window,
                ) == Some(workspace)
            {
                let settle = Settle {
                    workspace,
                    window,
                    candidate: Some(saved),
                    save: SaveFrame::Replace,
                };
                self.settle_floating(pass, settle, window_size);
            }
        }
        for window in self.active_floating_windows_in_workspace(window_store, pass.space) {
            let settle = Settle {
                workspace,
                window,
                candidate: None,
                save: SaveFrame::Replace,
            };
            self.settle_floating(pass, settle, window_size);
        }
    }

    /// Every window of a workspace not showing, parked. A floating one first has its frame saved,
    /// when its workspace has none for it, so it comes back where it was.
    fn parked_frames(
        &mut self,
        window_store: &WindowStore,
        pass: &mut FramePass,
        get_window_frame: &impl Fn(WindowId) -> Option<CGRect>,
        window_size: &impl Fn(WindowId) -> CGSize,
    ) {
        let hidden = self
            .virtual_workspace_manager
            .windows_in_inactive_workspaces(window_store, pass.space);
        for window in hidden {
            let frame = get_window_frame(window);
            if self.floating.is_floating(window)
                && let Some(workspace) = self.virtual_workspace_manager.workspace_for_window(
                    window_store,
                    pass.space,
                    window,
                )
            {
                let settle = Settle {
                    workspace,
                    window,
                    candidate: frame,
                    save: SaveFrame::IfAbsent,
                };
                self.settle_floating(pass, settle, window_size);
            }
            pass.frames.insert(window, parked_frame(pass.screen, frame, pass.all_screens));
        }
    }

    /// The window's floating frame in this pass, its candidate or the one it already has when
    /// usable and centred when not, saved as its frame in its workspace as `settle.save` says.
    fn settle_floating(
        &mut self,
        pass: &mut FramePass,
        settle: Settle,
        window_size: &impl Fn(WindowId) -> CGSize,
    ) {
        let Settle {
            workspace,
            window,
            candidate,
            save,
        } = settle;
        let candidate = candidate.or_else(|| pass.frames.get(&window).copied());
        let rect = usable_floating_frame(pass.screen, pass.all_screens, candidate)
            .unwrap_or_else(|| centred_on(pass.screen, window_size(window)));
        pass.frames.insert(window, rect);
        match save {
            SaveFrame::Replace => {
                self.floating_positions.store(pass.space, workspace, window, rect);
            }
            SaveFrame::IfAbsent => {
                self.floating_positions.store_if_absent(pass.space, workspace, window, rect);
            }
        }
    }

    /// Every frame `workspace_id` shows on `screen`, its strip spending what it held for the pass.
    pub fn calculate_layout_for_workspace(
        &mut self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        screen: CGRect,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let tiled = match self.workspace_layouts.active(space, workspace_id) {
            Some(layout) => self.lay_out_strip(workspace_id, layout, screen, gaps),
            None => Vec::new(),
        };
        self.with_floating_positions(window_store, space, workspace_id, tiled)
    }

    /// What `calculate_layout_for_workspace` answers, leaving every strip as it is: for a query,
    /// which only reads.
    pub fn preview_layout_for_workspace(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        screen: CGRect,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let tiled = match self.workspace_layouts.active(space, workspace_id) {
            Some(layout) => self.workspace_tree(workspace_id).preview_layout(
                layout,
                screen,
                &self.window_layout_constraints,
                &self.borders,
                gaps,
            ),
            None => Vec::new(),
        };
        self.with_floating_positions(window_store, space, workspace_id, tiled)
    }

    /// `tiled` with the floating windows of `workspace_id` at their stored positions.
    fn with_floating_positions(
        &self,
        window_store: &WindowStore,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        tiled: Vec<(WindowId, CGRect)>,
    ) -> Vec<(WindowId, CGRect)> {
        let mut positions: HashMap<WindowId, CGRect> = tiled.into_iter().collect();
        let floating_positions = self.floating_positions.workspace_positions(space, workspace_id);
        for (window_id, stored_position) in floating_positions {
            if self.floating.is_floating(window_id)
                && self.virtual_workspace_manager.workspace_for_window(
                    window_store,
                    space,
                    window_id,
                ) == Some(workspace_id)
            {
                positions.insert(window_id, stored_position);
            }
        }
        positions.into_iter().collect()
    }

    /// `ws_id`'s strip `layout` laid out on `screen`, spending what it held for the pass.
    fn lay_out_strip(
        &mut self,
        ws_id: VirtualWorkspaceId,
        layout: LayoutId,
        screen: CGRect,
        gaps: &Gaps,
    ) -> Vec<(WindowId, CGRect)> {
        let constraints = &self.window_layout_constraints;
        let system = &mut self.virtual_workspace_manager.workspaces[ws_id].layout_system;
        system.calculate_layout(layout, screen, constraints, &self.borders, gaps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::settings::Edges;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    }

    fn screen() -> CGRect { rect(0.0, 0.0, 1000.0, 800.0) }

    #[test]
    fn a_column_on_screen_is_written_where_the_strip_puts_it() {
        let column = rect(-200.0, 0.0, 500.0, 800.0);
        assert_eq!(strip_frame(screen(), column, &[screen()]), column);
    }

    /// A column off the strip parks in the bottom corner on the side it went off, so an animation
    /// can bring it back in from that edge.
    #[test]
    fn a_column_off_the_strip_parks_in_the_corner_on_its_side() {
        let left = strip_frame(screen(), rect(-600.0, 0.0, 500.0, 800.0), &[screen()]);
        let right = strip_frame(screen(), rect(1100.0, 0.0, 500.0, 800.0), &[screen()]);
        assert_eq!(left, rect(-499.0, 799.0, 500.0, 800.0));
        assert_eq!(right, rect(999.0, 799.0, 500.0, 800.0));
    }

    #[test]
    fn a_floating_frame_is_kept_only_while_enough_of_it_shows_to_grab() {
        let usable = |candidate| usable_floating_frame(screen(), &[screen()], candidate);
        let shown = rect(100.0, 100.0, 300.0, 200.0);
        assert_eq!(usable(Some(shown)), Some(shown));
        assert_eq!(usable(None), None);
        assert_eq!(usable(Some(rect(999.0, 799.0, 300.0, 200.0))), None);
        assert_eq!(usable(Some(rect(970.0, 100.0, 300.0, 200.0))), None);
        assert_eq!(usable(Some(rect(100.0, 770.0, 300.0, 200.0))), None);
    }

    #[test]
    fn a_floating_window_with_no_usable_frame_is_centred() {
        assert_eq!(
            centred_on(screen(), CGSize::new(200.0, 100.0)),
            rect(400.0, 350.0, 200.0, 100.0)
        );
    }

    #[test]
    fn a_maximized_floating_window_fills_the_screen_inside_the_outer_gaps() {
        let gaps = Gaps {
            outer: Edges {
                top: 10.0,
                left: 20.0,
                bottom: 40.0,
                right: 30.0,
            },
            inner: 8.0,
        };
        assert_eq!(maximized_frame(screen(), &gaps), rect(20.0, 10.0, 950.0, 750.0));
    }

    /// A hidden window parks at its own size, and one whose frame cannot be read parks as an
    /// unknown-sized one would.
    #[test]
    fn a_window_of_a_hidden_workspace_parks_bottom_right_at_its_size() {
        let known = parked_frame(screen(), Some(rect(100.0, 100.0, 300.0, 200.0)), &[screen()]);
        let unknown = parked_frame(screen(), None, &[screen()]);
        assert_eq!(known, rect(999.0, 799.0, 300.0, 200.0));
        assert_eq!(unknown, CGRect::new(CGPoint::new(999.0, 799.0), UNKNOWN_SIZE));
    }
}
