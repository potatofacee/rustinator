use egui;

use crate::pane::PaneId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Direction {
    /// Divider is horizontal; children are stacked top / bottom.
    Horizontal,
    /// Divider is vertical; children are side by side.
    Vertical,
}

pub enum Node {
    Leaf(PaneId),
    Split {
        dir: Direction,
        /// Fraction of the parent rect assigned to `left` (or `top`).
        ratio: f32,
        left: Box<Node>,
        right: Box<Node>,
    },
}

pub enum RemoveResult {
    NotFound,
    Found,
    /// This subtree is empty after the removal and should be handled by the caller.
    Empty,
}

#[derive(Debug, Clone)]
pub struct DividerHandle {
    pub path: Vec<u8>,
    pub rect: egui::Rect,
    pub parent_rect: egui::Rect,
    pub dir: Direction,
}

impl Node {
    /// Find the leaf `target` and replace it with a split of (target, new_id).
    /// Returns true if the split was performed.
    pub fn split_leaf(&mut self, target: PaneId, new_id: PaneId, dir: Direction) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split {
                    dir,
                    ratio: 0.5,
                    left: Box::new(Node::Leaf(target)),
                    right: Box::new(Node::Leaf(new_id)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { left, right, .. } => {
                left.split_leaf(target, new_id, dir) || right.split_leaf(target, new_id, dir)
            }
        }
    }

    /// Remove the leaf `target`. Collapses the parent split to the remaining sibling.
    pub fn remove_leaf(&mut self, target: PaneId) -> RemoveResult {
        match self {
            Node::Leaf(id) if *id == target => RemoveResult::Empty,
            Node::Leaf(_) => RemoveResult::NotFound,
            Node::Split { left, right, .. } => {
                match left.remove_leaf(target) {
                    RemoveResult::Empty => {
                        // Left is gone; promote right in place of self.
                        let promoted = std::mem::replace(right.as_mut(), Node::Leaf(0));
                        *self = promoted;
                        RemoveResult::Found
                    }
                    RemoveResult::Found => RemoveResult::Found,
                    RemoveResult::NotFound => match right.remove_leaf(target) {
                        RemoveResult::Empty => {
                            let promoted = std::mem::replace(left.as_mut(), Node::Leaf(0));
                            *self = promoted;
                            RemoveResult::Found
                        }
                        r => r,
                    },
                }
            }
        }
    }

    /// Walk the tree, computing each leaf's rect given the top-level rect and a gap between panes.
    pub fn walk_rects(&self, rect: egui::Rect, gap: f32, out: &mut Vec<(PaneId, egui::Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, rect)),
            Node::Split {
                dir,
                ratio,
                left,
                right,
            } => {
                let half = gap / 2.0;
                let (a, b) = match dir {
                    Direction::Horizontal => {
                        let split_y = rect.top() + rect.height() * ratio;
                        let top = egui::Rect::from_min_max(
                            rect.min,
                            egui::pos2(rect.right(), (split_y - half).max(rect.top())),
                        );
                        let bot = egui::Rect::from_min_max(
                            egui::pos2(rect.left(), (split_y + half).min(rect.bottom())),
                            rect.max,
                        );
                        (top, bot)
                    }
                    Direction::Vertical => {
                        let split_x = rect.left() + rect.width() * ratio;
                        let l = egui::Rect::from_min_max(
                            rect.min,
                            egui::pos2((split_x - half).max(rect.left()), rect.bottom()),
                        );
                        let r = egui::Rect::from_min_max(
                            egui::pos2((split_x + half).min(rect.right()), rect.top()),
                            rect.max,
                        );
                        (l, r)
                    }
                };
                left.walk_rects(a, gap, out);
                right.walk_rects(b, gap, out);
            }
        }
    }

    /// Walk the tree collecting one hit-test rect per internal Split so the
    /// user can drag to resize. Each handle carries the path through the tree
    /// and the parent rect so the caller can translate a new pointer position
    /// back into a ratio.
    pub fn walk_dividers(
        &self,
        rect: egui::Rect,
        gap: f32,
        out: &mut Vec<DividerHandle>,
    ) {
        self.walk_dividers_impl(rect, gap, Vec::new(), out);
    }

    fn walk_dividers_impl(
        &self,
        rect: egui::Rect,
        gap: f32,
        path: Vec<u8>,
        out: &mut Vec<DividerHandle>,
    ) {
        if let Node::Split {
            dir,
            ratio,
            left,
            right,
        } = self
        {
            // Widen the divider hit area a bit so it's graspable even when `gap` is tiny.
            let hit = gap.max(6.0);
            let half = hit / 2.0;
            let (divider_rect, a, b) = match dir {
                Direction::Horizontal => {
                    let split_y = rect.top() + rect.height() * ratio;
                    let divider = egui::Rect::from_min_max(
                        egui::pos2(rect.left(), (split_y - half).max(rect.top())),
                        egui::pos2(rect.right(), (split_y + half).min(rect.bottom())),
                    );
                    let half_gap = gap / 2.0;
                    let top = egui::Rect::from_min_max(
                        rect.min,
                        egui::pos2(rect.right(), (split_y - half_gap).max(rect.top())),
                    );
                    let bot = egui::Rect::from_min_max(
                        egui::pos2(rect.left(), (split_y + half_gap).min(rect.bottom())),
                        rect.max,
                    );
                    (divider, top, bot)
                }
                Direction::Vertical => {
                    let split_x = rect.left() + rect.width() * ratio;
                    let divider = egui::Rect::from_min_max(
                        egui::pos2((split_x - half).max(rect.left()), rect.top()),
                        egui::pos2((split_x + half).min(rect.right()), rect.bottom()),
                    );
                    let half_gap = gap / 2.0;
                    let l = egui::Rect::from_min_max(
                        rect.min,
                        egui::pos2((split_x - half_gap).max(rect.left()), rect.bottom()),
                    );
                    let r = egui::Rect::from_min_max(
                        egui::pos2((split_x + half_gap).min(rect.right()), rect.top()),
                        rect.max,
                    );
                    (divider, l, r)
                }
            };
            out.push(DividerHandle {
                path: path.clone(),
                rect: divider_rect,
                parent_rect: rect,
                dir: *dir,
            });
            let mut lp = path.clone();
            lp.push(0);
            let mut rp = path;
            rp.push(1);
            left.walk_dividers_impl(a, gap, lp, out);
            right.walk_dividers_impl(b, gap, rp, out);
        }
    }

    /// Set the ratio of the Split at the given path, clamped to a sane range.
    pub fn set_ratio(&mut self, path: &[u8], new_ratio: f32) {
        let clamped = new_ratio.clamp(0.05, 0.95);
        let mut node: &mut Node = self;
        for step in path {
            match node {
                Node::Split { left, right, .. } => {
                    node = if *step == 0 { left.as_mut() } else { right.as_mut() };
                }
                Node::Leaf(_) => return,
            }
        }
        if let Node::Split { ratio, .. } = node {
            *ratio = clamped;
        }
    }

    /// Set the ratio of the Split at the given path while guaranteeing both
    /// children keep at least `min_cells` cells along the split axis.
    ///
    /// `container_extent_px` is the pixel size of the split's container along
    /// the split axis (width for a Vertical split, height for a Horizontal
    /// split). `cell_extent_px` is the cell size along that same axis. The
    /// allowable ratio range is derived so neither child drops below
    /// `min_cells`, then we still clamp to the original 0.05–0.95 sanity band.
    pub fn set_ratio_min_cells(
        &mut self,
        path: &[u8],
        new_ratio: f32,
        container_extent_px: f32,
        cell_extent_px: f32,
        min_cells: f32,
    ) {
        let min_ratio;
        let max_ratio;
        if container_extent_px > 0.0 && cell_extent_px > 0.0 {
            // Fraction of the container that `min_cells` occupies.
            let min_frac = (min_cells * cell_extent_px / container_extent_px).clamp(0.0, 0.5);
            min_ratio = min_frac.max(0.05);
            max_ratio = (1.0 - min_frac).min(0.95);
        } else {
            min_ratio = 0.05;
            max_ratio = 0.95;
        }
        // If the container is too small to host two min-size children, fall
        // back to a centred split rather than producing an inverted range.
        let clamped = if min_ratio > max_ratio {
            0.5
        } else {
            new_ratio.clamp(min_ratio, max_ratio)
        };
        self.set_ratio(path, clamped);
    }

    /// Swap two leaf pane IDs in the tree.
    pub fn swap_leaves(&mut self, a: PaneId, b: PaneId) {
        Self::swap_leaves_impl(self, a, b);
    }

    fn swap_leaves_impl(node: &mut Node, a: PaneId, b: PaneId) {
        match node {
            Node::Leaf(id) => {
                if *id == a {
                    *id = b;
                } else if *id == b {
                    *id = a;
                }
            }
            Node::Split { left, right, .. } => {
                Self::swap_leaves_impl(left, a, b);
                Self::swap_leaves_impl(right, a, b);
            }
        }
    }

    /// Collect all pane ids in left-to-right, top-to-bottom traversal order (useful for cycling).
    pub fn leaves_in_order(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { left, right, .. } => {
                left.leaves_in_order(out);
                right.leaves_in_order(out);
            }
        }
    }

    /// Rotate the layout clockwise: flip all split directions and swap children.
    pub fn rotate_cw(&mut self) {
        match self {
            Node::Leaf(_) => {}
            Node::Split { dir, left, right, .. } => {
                *dir = match dir {
                    Direction::Horizontal => Direction::Vertical,
                    Direction::Vertical => Direction::Horizontal,
                };
                std::mem::swap(left, right);
                left.rotate_cw();
                right.rotate_cw();
            }
        }
    }

    /// Rotate the layout counter-clockwise: flip all split directions (don't swap children).
    pub fn rotate_ccw(&mut self) {
        match self {
            Node::Leaf(_) => {}
            Node::Split { dir, left, right, .. } => {
                *dir = match dir {
                    Direction::Horizontal => Direction::Vertical,
                    Direction::Vertical => Direction::Horizontal,
                };
                left.rotate_ccw();
                right.rotate_ccw();
            }
        }
    }

    /// Equalize this split to 50/50. Affects only the top-level split; nested
    /// splits keep their ratios. No-op on a leaf.
    /// Recursively equalize every split in the tree to 50/50.
    pub fn rebalance_recursive(&mut self) {
        if let Node::Split { ratio, left, right, .. } = self {
            *ratio = 0.5;
            left.rebalance_recursive();
            right.rebalance_recursive();
        }
    }
}

/// Move a split divider by `step` whole cells and return the resulting ratio.
///
/// `container_px` is the container's pixel extent along the split axis and
/// `cell_px` the cell extent along that same axis, so `total = container/cell`
/// is the cell count. The current boundary (in cells) is stepped by `step`,
/// clamped so neither child drops below `min_cells`, then converted back to a
/// ratio. Degenerate containers (`total <= 0`, or too small to host two
/// `min_cells` children) return `current_ratio` unchanged.
pub fn ratio_after_cell_step(
    current_ratio: f32,
    step: i32,
    container_px: f32,
    cell_px: f32,
    min_cells: f32,
) -> f32 {
    if cell_px <= 0.0 {
        return current_ratio;
    }
    let total = container_px / cell_px;
    if total <= 0.0 || 2.0 * min_cells > total {
        return current_ratio;
    }
    let cur = (current_ratio * total).round();
    let new = (cur + step as f32).clamp(min_cells, total - min_cells);
    new / total
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum LayoutTemplate {
    Terminal,
    Split {
        dir: Direction,
        ratio: f32,
        left: Box<LayoutTemplate>,
        right: Box<LayoutTemplate>,
    },
}

impl Node {
    pub fn to_template(&self) -> LayoutTemplate {
        match self {
            Node::Leaf(_) => LayoutTemplate::Terminal,
            Node::Split { dir, ratio, left, right } => LayoutTemplate::Split {
                dir: *dir,
                ratio: *ratio,
                left: Box::new(left.to_template()),
                right: Box::new(right.to_template()),
            },
        }
    }
}

impl LayoutTemplate {
    pub fn leaf_count(&self) -> usize {
        match self {
            LayoutTemplate::Terminal => 1,
            LayoutTemplate::Split { left, right, .. } => left.leaf_count() + right.leaf_count(),
        }
    }

    pub fn build(&self, ids: &mut impl Iterator<Item = PaneId>) -> Node {
        match self {
            LayoutTemplate::Terminal => Node::Leaf(ids.next().unwrap_or(0)),
            LayoutTemplate::Split { dir, ratio, left, right } => Node::Split {
                dir: *dir,
                ratio: *ratio,
                left: Box::new(left.build(ids)),
                right: Box::new(right.build(ids)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(node: &Node) -> Vec<PaneId> {
        let mut out = Vec::new();
        node.leaves_in_order(&mut out);
        out
    }

    #[test]
    fn split_creates_two_leaves() {
        let mut root = Node::Leaf(1);
        assert!(root.split_leaf(1, 2, Direction::Vertical));
        assert_eq!(leaves(&root), vec![1, 2]);
    }

    #[test]
    fn split_nested_leaf() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        assert!(root.split_leaf(2, 3, Direction::Horizontal));
        assert_eq!(leaves(&root), vec![1, 2, 3]);
    }

    #[test]
    fn split_nonexistent_is_noop() {
        let mut root = Node::Leaf(1);
        assert!(!root.split_leaf(99, 2, Direction::Vertical));
        assert_eq!(leaves(&root), vec![1]);
    }

    #[test]
    fn remove_collapses_parent_to_sibling() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        match root.remove_leaf(1) {
            RemoveResult::Found => {}
            _ => panic!("expected Found"),
        }
        assert_eq!(leaves(&root), vec![2]);
    }

    #[test]
    fn remove_last_empties_tree() {
        let mut root = Node::Leaf(1);
        match root.remove_leaf(1) {
            RemoveResult::Empty => {}
            _ => panic!("expected Empty"),
        }
    }

    #[test]
    fn remove_missing_is_notfound() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        match root.remove_leaf(99) {
            RemoveResult::NotFound => {}
            _ => panic!("expected NotFound"),
        }
        assert_eq!(leaves(&root), vec![1, 2]);
    }

    #[test]
    fn walk_rects_splits_width_50_50() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        let rect = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(100.0, 60.0),
        );
        let mut out = Vec::new();
        root.walk_rects(rect, 0.0, &mut out);
        assert_eq!(out.len(), 2);
        // Left pane is id=1, right is id=2; both 50px wide before any drag.
        let (_, left_rect) = out[0];
        let (_, right_rect) = out[1];
        assert!((left_rect.width() - 50.0).abs() < 0.01);
        assert!((right_rect.width() - 50.0).abs() < 0.01);
    }

    #[test]
    fn walk_rects_horizontal_splits_height() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Horizontal);
        let rect = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(100.0, 60.0),
        );
        let mut out = Vec::new();
        root.walk_rects(rect, 0.0, &mut out);
        let (_, top) = out[0];
        let (_, bot) = out[1];
        assert!((top.height() - 30.0).abs() < 0.01);
        assert!((bot.height() - 30.0).abs() < 0.01);
    }

    // ── Gap inventory guardrails ──────────────────────────────────────

    #[test]
    fn rotate_cw_swaps_children() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        root.rotate_cw();
        // After CW rotation: Vertical -> Horizontal, children swapped (2, 1)
        let out = leaves(&root);
        assert_eq!(out, vec![2, 1]);
        match &root {
            Node::Split { dir, .. } => assert_eq!(*dir, Direction::Horizontal),
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn rotate_ccw_swaps_children() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        root.rotate_ccw();
        // After CCW rotation: Vertical -> Horizontal, children NOT swapped (1, 2)
        let out = leaves(&root);
        assert_eq!(out, vec![1, 2]);
        match &root {
            Node::Split { dir, .. } => assert_eq!(*dir, Direction::Horizontal),
            _ => panic!("expected split"),
        }
    }

    /// Build Vertical(Leaf1 | Horizontal(Leaf2, Leaf3)) with off-centre ratios:
    /// root ratio 0.3, nested ratio 0.7.
    fn skewed_nested_tree() -> Node {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        root.split_leaf(2, 3, Direction::Horizontal);
        root.set_ratio(&[], 0.3);
        root.set_ratio(&[1], 0.7);
        root
    }

    fn nested_ratio(node: &Node) -> f32 {
        match node {
            Node::Split { right, .. } => match right.as_ref() {
                Node::Split { ratio, .. } => *ratio,
                _ => panic!("expected nested split on the right"),
            },
            _ => panic!("expected split at root"),
        }
    }

    // Gap #45: recursive rebalance (equalize all nested ratios).
    #[test]
    fn rebalance_recursive_equalizes_nested() {
        let mut root = skewed_nested_tree();
        root.rebalance_recursive();
        // Every split in the tree is now 0.5.
        assert!((root_ratio(&root) - 0.5).abs() < 1e-6);
        assert!((nested_ratio(&root) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn ratio_after_cell_step_moves_one_cell() {
        // 100px container / 10px cells = 10 cells; ratio 0.5 => boundary at 5.
        let up = ratio_after_cell_step(0.5, 1, 100.0, 10.0, 1.0);
        assert!((up - 0.6).abs() < 1e-6);
        let down = ratio_after_cell_step(0.5, -1, 100.0, 10.0, 1.0);
        assert!((down - 0.4).abs() < 1e-6);
    }

    #[test]
    fn ratio_after_cell_step_clamps_to_min_cells() {
        // Stepping down past the floor pins the boundary at min_cells = 2 of 10.
        let r = ratio_after_cell_step(0.3, -5, 100.0, 10.0, 2.0);
        assert!((r - 0.2).abs() < 1e-6);
        // Stepping up past the ceiling pins it at total - min_cells = 8 of 10.
        let r = ratio_after_cell_step(0.7, 5, 100.0, 10.0, 2.0);
        assert!((r - 0.8).abs() < 1e-6);
    }

    #[test]
    fn ratio_after_cell_step_degenerate_returns_input() {
        // total <= 0 (zero container) returns the input ratio untouched.
        assert!((ratio_after_cell_step(0.42, 1, 0.0, 10.0, 1.0) - 0.42).abs() < 1e-6);
        // Zero cell size is also degenerate.
        assert!((ratio_after_cell_step(0.42, 1, 100.0, 0.0, 1.0) - 0.42).abs() < 1e-6);
        // Container too small to host two min-size children (2*2 > 3 cells).
        assert!((ratio_after_cell_step(0.42, 1, 30.0, 10.0, 2.0) - 0.42).abs() < 1e-6);
    }

    // Gap #4 (partial): layout template stores per-terminal metadata
    #[test]
    #[ignore = "gap #4 partial: LayoutTemplate doesn't store per-terminal cwd/command/group"]
    fn layout_template_with_metadata() {
        panic!("extend LayoutTemplate::Terminal to carry optional cwd, command, group, profile");
    }

    // ── Block B: ratio / divider / rect / remove_leaf gaps ────────────

    /// Read the ratio of the top-level Split (panics if the root is a leaf).
    fn root_ratio(node: &Node) -> f32 {
        match node {
            Node::Split { ratio, .. } => *ratio,
            _ => panic!("expected split at root"),
        }
    }

    #[test]
    fn set_ratio_clamps_out_of_range() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);

        // Above the upper bound clamps to 0.95.
        root.set_ratio(&[], 2.0);
        assert!((root_ratio(&root) - 0.95).abs() < 1e-6);

        // Below the lower bound clamps to 0.05.
        root.set_ratio(&[], -1.0);
        assert!((root_ratio(&root) - 0.05).abs() < 1e-6);

        // An in-range value passes through untouched.
        root.set_ratio(&[], 0.42);
        assert!((root_ratio(&root) - 0.42).abs() < 1e-6);
    }

    #[test]
    fn set_ratio_min_cells_clamps_band_from_pixels() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);

        // container 100px, cell 10px, min 2 cells => min_frac = 0.2,
        // so the allowed band is [0.20, 0.80].
        let container = 100.0;
        let cell = 10.0;
        let min_cells = 2.0;

        // Request below the band -> clamped up to 0.20.
        root.set_ratio_min_cells(&[], 0.01, container, cell, min_cells);
        assert!((root_ratio(&root) - 0.20).abs() < 1e-6);

        // Request above the band -> clamped down to 0.80.
        root.set_ratio_min_cells(&[], 0.99, container, cell, min_cells);
        assert!((root_ratio(&root) - 0.80).abs() < 1e-6);

        // Request inside the band passes through.
        root.set_ratio_min_cells(&[], 0.50, container, cell, min_cells);
        assert!((root_ratio(&root) - 0.50).abs() < 1e-6);
    }

    #[test]
    fn set_ratio_min_cells_degenerate_container_falls_back_to_center() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);

        // container 10px but each min child wants 2 cells * 10px = 20px,
        // i.e. larger than the whole container. The band collapses to the
        // centre, so any requested ratio lands at 0.5.
        let container = 10.0;
        let cell = 10.0;
        let min_cells = 2.0;

        root.set_ratio_min_cells(&[], 0.10, container, cell, min_cells);
        assert!((root_ratio(&root) - 0.5).abs() < 1e-6);

        root.set_ratio_min_cells(&[], 0.90, container, cell, min_cells);
        assert!((root_ratio(&root) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn set_ratio_walks_path_and_noops_on_leaf() {
        // root: Vertical(Leaf1 | Horizontal(Leaf2, Leaf3))
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        root.split_leaf(2, 3, Direction::Horizontal);

        // Walk to the nested split via path [1] and set its ratio.
        root.set_ratio(&[1], 0.30);
        match &root {
            Node::Split { ratio, right, .. } => {
                // Root ratio untouched.
                assert!((ratio - 0.5).abs() < 1e-6);
                match right.as_ref() {
                    Node::Split { ratio: inner, .. } => {
                        assert!((inner - 0.30).abs() < 1e-6);
                    }
                    _ => panic!("expected nested split on the right"),
                }
            }
            _ => panic!("expected split at root"),
        }

        // Path pointing at a leaf is a no-op (no panic, ratios unchanged).
        root.set_ratio(&[0], 0.10);
        assert!((root_ratio(&root) - 0.5).abs() < 1e-6);

        // set_ratio on a bare leaf root is a no-op.
        let mut leaf = Node::Leaf(7);
        leaf.set_ratio(&[], 0.25);
        assert!(matches!(leaf, Node::Leaf(7)));
    }

    #[test]
    fn walk_dividers_paths_and_vertical_center() {
        // root: Vertical(Leaf1 | Horizontal(Leaf2, Leaf3))
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        root.split_leaf(2, 3, Direction::Horizontal);

        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 60.0));
        let mut out = Vec::new();
        root.walk_dividers(rect, 0.0, &mut out);

        // Two internal splits -> two handles.
        assert_eq!(out.len(), 2);

        // The root divider comes first with an empty path and is vertical,
        // centred at x = 50 across the full height.
        assert_eq!(out[0].path, Vec::<u8>::new());
        assert_eq!(out[0].dir, Direction::Vertical);
        assert!((out[0].rect.center().x - 50.0).abs() < 0.01);

        // The nested divider lives down the right branch -> path [1].
        assert_eq!(out[1].path, vec![1u8]);
        assert_eq!(out[1].dir, Direction::Horizontal);
    }

    #[test]
    fn walk_rects_vertical_gap_channel() {
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);

        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 60.0));
        let gap = 10.0;
        let mut out = Vec::new();
        root.walk_rects(rect, gap, &mut out);

        assert_eq!(out.len(), 2);
        let (_, left_rect) = out[0];
        let (_, right_rect) = out[1];

        // Split at x = 50; each side recedes by gap/2 = 5px, leaving a 10px
        // empty channel between them.
        assert!((left_rect.right() - 45.0).abs() < 0.01);
        assert!((right_rect.left() - 55.0).abs() < 0.01);
        assert!((right_rect.left() - left_rect.right() - gap).abs() < 0.01);
    }

    #[test]
    fn remove_leaf_promotes_multileaf_sibling_subtree() {
        // root: Vertical(Leaf1 | Horizontal(Leaf2, Leaf3))
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Direction::Vertical);
        root.split_leaf(2, 3, Direction::Horizontal);

        // Removing the single left leaf promotes the *multi-leaf* right
        // subtree to the root, preserving its structure.
        match root.remove_leaf(1) {
            RemoveResult::Found => {}
            _ => panic!("expected Found"),
        }
        assert_eq!(leaves(&root), vec![2, 3]);
        match &root {
            Node::Split { dir, .. } => assert_eq!(*dir, Direction::Horizontal),
            _ => panic!("expected the multi-leaf subtree to be promoted, got a leaf"),
        }
    }
}
