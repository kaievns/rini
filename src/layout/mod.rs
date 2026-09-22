//! The scrolling layout: a strip of columns of windows and the operations on it.
//!
//! Pure geometry over window ids and frames; knows nothing about workspaces or spaces, and has no
//! `platform` because it touches nothing outside itself. `ScrollingLayoutSystem` is the seam every
//! other feature reaches it through. See `src/layout/docs/strip.md`.

pub use rini_ipc::protocol::{Direction, ResizeOrientation};

pub mod domain;
pub mod settings;

pub use domain::scrolling::ScrollingLayoutSystem;

slotmap::new_key_type! { pub struct LayoutId; }

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WindowLayoutConstraints {
    pub is_resizable: bool,
    pub locked_width: f64,
    pub locked_height: f64,
    pub min_width: f64,
    pub min_height: f64,
    pub max_width: f64,
    pub max_height: f64,
}

impl WindowLayoutConstraints {
    pub fn normalized(self) -> Self {
        let clean = |v: f64| if v.is_finite() { v.max(0.0) } else { 0.0 };
        let min_width = clean(self.min_width);
        let min_height = clean(self.min_height);
        let mut max_width = clean(self.max_width);
        let mut max_height = clean(self.max_height);
        if max_width > 0.0 && max_width < min_width {
            max_width = min_width;
        }
        if max_height > 0.0 && max_height < min_height {
            max_height = min_height;
        }
        Self {
            is_resizable: self.is_resizable,
            locked_width: clean(self.locked_width),
            locked_height: clean(self.locked_height),
            min_width,
            min_height,
            max_width,
            max_height,
        }
    }

    /// Whether these limits can change a frame at all.
    ///
    /// A window that reports nothing is the common case, and discovering one must not ask for a
    /// layout pass. A window that reports a minimum wider than the default column must, because
    /// the limits arrive from the window server after Accessibility has already had the window
    /// placed. See "Column width" in `src/layout/docs/strip.md`.
    pub fn constrains_layout(self) -> bool {
        let c = self.normalized();
        c.min_width > 0.0
            || c.min_height > 0.0
            || c.max_width > 0.0
            || c.max_height > 0.0
            || c.locked_width > 0.0
            || c.locked_height > 0.0
    }

    pub fn min_for_axis(self, horizontal: bool) -> f64 {
        if horizontal {
            self.min_width
        } else {
            self.min_height
        }
    }

    pub fn max_for_axis(self, horizontal: bool) -> f64 {
        if horizontal {
            self.max_width
        } else {
            self.max_height
        }
    }

    pub fn fixed_for_axis(self, horizontal: bool) -> Option<f64> {
        let locked = if horizontal {
            self.locked_width
        } else {
            self.locked_height
        };
        let min = self.min_for_axis(horizontal);
        let max = self.max_for_axis(horizontal);
        // Axis-specific lock: when min/max collapse to the same positive value,
        // treat that axis as fixed even if the window is generally resizable.
        if min > 0.0 && max > 0.0 && (min - max).abs() <= f64::EPSILON {
            return Some(max);
        }
        if !self.is_resizable {
            return (locked > 0.0).then_some(locked);
        }
        None
    }

    pub fn resizable_for_axis(self, horizontal: bool) -> bool {
        self.fixed_for_axis(horizontal).is_none()
    }

    pub fn resizable_any_axis(self) -> bool {
        self.resizable_for_axis(true) || self.resizable_for_axis(false)
    }
}


#[cfg(test)]
mod tests {
    use super::{ScrollingLayoutSystem, WindowLayoutConstraints};
    use rini_core::ids::WindowId;
    use crate::layout::settings::{ScrollingLayoutSettings, WindowInsertionPoint};

    fn w(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    #[test]
    fn insertion_point_is_honoured_by_the_scrolling_layout() {
        let mut scrolling_settings = ScrollingLayoutSettings::default();
        scrolling_settings.base.window_insertion_point = Some(WindowInsertionPoint::EndOfTree);
        let mut scrolling = ScrollingLayoutSystem::new(&scrolling_settings);
        let scrolling_layout = scrolling.create_layout();
        scrolling.add_window_after_selection(scrolling_layout, w(1));
        scrolling.add_window_after_selection(scrolling_layout, w(2));
        scrolling.select_window(scrolling_layout, w(1));
        // EndOfTree ignores the selection and appends, so w(3) lands last rather than
        // directly after w(1).
        scrolling.add_window_after_selection(scrolling_layout, w(3));
        assert_eq!(
            scrolling.all_windows_in_layout(scrolling_layout),
            vec![w(1), w(2), w(3)]
        );
    }

    #[test]
    fn axis_specific_fixed_detection_supports_one_axis_locked_other_resizable() {
        let c = WindowLayoutConstraints {
            is_resizable: true,
            locked_width: 700.0,
            locked_height: 400.0,
            min_width: 723.0,
            min_height: 470.0,
            max_width: 723.0,
            max_height: 0.0,
        }
        .normalized();

        assert_eq!(c.fixed_for_axis(true), Some(723.0));
        assert_eq!(c.fixed_for_axis(false), None);
        assert!(!c.resizable_for_axis(true));
        assert!(c.resizable_for_axis(false));
        assert!(c.resizable_any_axis());
    }

    #[test]
    fn non_resizable_zero_locked_size_is_not_treated_as_fixed() {
        let c = WindowLayoutConstraints {
            is_resizable: false,
            locked_width: 0.0,
            locked_height: 0.0,
            min_width: 0.0,
            min_height: 0.0,
            max_width: 0.0,
            max_height: 0.0,
        }
        .normalized();

        assert_eq!(c.fixed_for_axis(true), None);
        assert_eq!(c.fixed_for_axis(false), None);
        assert!(c.resizable_for_axis(true));
        assert!(c.resizable_for_axis(false));
    }

    #[test]
    fn non_resizable_positive_locked_size_remains_fixed() {
        let c = WindowLayoutConstraints {
            is_resizable: false,
            locked_width: 640.0,
            locked_height: 360.0,
            min_width: 0.0,
            min_height: 0.0,
            max_width: 0.0,
            max_height: 0.0,
        }
        .normalized();

        assert_eq!(c.fixed_for_axis(true), Some(640.0));
        assert_eq!(c.fixed_for_axis(false), Some(360.0));
        assert!(!c.resizable_for_axis(true));
        assert!(!c.resizable_for_axis(false));
        assert!(!c.resizable_any_axis());
    }

    #[test]
    fn positive_max_only_constraint_is_not_treated_as_fixed() {
        let c = WindowLayoutConstraints {
            is_resizable: true,
            locked_width: 0.0,
            locked_height: 0.0,
            min_width: 0.0,
            min_height: 0.0,
            max_width: 600.0,
            max_height: 480.0,
        }
        .normalized();

        assert_eq!(c.fixed_for_axis(true), None);
        assert_eq!(c.fixed_for_axis(false), None);
        assert_eq!(c.max_for_axis(true), 600.0);
        assert_eq!(c.max_for_axis(false), 480.0);
        assert!(c.resizable_for_axis(true));
        assert!(c.resizable_for_axis(false));
    }

    fn window_nodes(
        tree: &rini_ipc::protocol::ContainerTreeNode,
    ) -> Vec<&rini_ipc::protocol::ContainerTreeNode> {
        let mut windows = Vec::new();
        if tree.node_type == rini_ipc::protocol::ContainerNodeType::Window {
            windows.push(tree);
        }
        for child in &tree.children {
            windows.extend(window_nodes(child));
        }
        windows
    }

    /// Upstream's container-tree test, reduced to the one layout system that still exists.
    ///
    /// It originally also covered Traditional, Bsp and MasterStack. Those systems were
    /// removed in "refactor: remove the tree-based layout modes, leaving only scrolling", so
    /// their sections are gone rather than the whole test: the scrolling assertions are
    /// unaffected by that removal and still pin the container-tree contract the IPC layer
    /// depends on.
    #[test]
    fn normalized_container_trees_expose_layout_topology() {
        let mut scrolling = ScrollingLayoutSystem::default();
        let layout = scrolling.create_layout();
        scrolling.add_window_after_selection(layout, w(1));
        scrolling.add_window_after_selection(layout, w(2));
        let tree = scrolling.container_tree(layout);
        assert_eq!(tree.node_type, rini_ipc::protocol::ContainerNodeType::Container);
        assert!(tree.children.iter().all(|node| {
            node.layout_kind == Some(rini_ipc::protocol::LayoutKind::Vertical)
        }));
        assert_eq!(window_nodes(&tree).len(), 2);
        assert_eq!(
            window_nodes(&tree).iter().filter(|node| node.is_selected).count(),
            1
        );
    }
}

#[cfg(test)]
mod constrains_layout_tests {
    use super::WindowLayoutConstraints;

    #[test]
    fn a_window_that_reports_nothing_constrains_nothing() {
        assert!(!WindowLayoutConstraints::default().constrains_layout());
        assert!(
            !WindowLayoutConstraints { is_resizable: true, ..Default::default() }
                .constrains_layout(),
            "being resizable is not a limit on its own"
        );
    }

    // The ACME case: a minimum wider than the default column. Learning this has to be worth a
    // layout pass, or the window stays clipped at the default width until something else recomputes.
    #[test]
    fn a_minimum_width_constrains_the_layout() {
        let acme = WindowLayoutConstraints {
            is_resizable: true,
            min_width: 1800.0,
            ..Default::default()
        };
        assert!(acme.constrains_layout());
    }

    #[test]
    fn a_locked_or_maximum_size_constrains_the_layout() {
        for c in [
            WindowLayoutConstraints { locked_width: 300.0, ..Default::default() },
            WindowLayoutConstraints { locked_height: 200.0, ..Default::default() },
            WindowLayoutConstraints { max_width: 900.0, ..Default::default() },
            WindowLayoutConstraints { min_height: 120.0, ..Default::default() },
        ] {
            assert!(c.constrains_layout(), "{c:?}");
        }
    }
}
