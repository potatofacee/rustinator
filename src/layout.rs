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

    // Gap #45: rebalance dividers (equalize ratios)
    // When implementing: add Node::rebalance() that sets ratio to 0.5.
    #[test]
    #[ignore = "gap #45: Node::rebalance not yet implemented"]
    fn rebalance_equalizes_ratios() {
        panic!("implement Node::rebalance(): set this split's ratio to 0.5");
    }

    // Gap #45: recursive rebalance (equalize all nested ratios)
    #[test]
    #[ignore = "gap #45: Node::rebalance_recursive not yet implemented"]
    fn rebalance_recursive_equalizes_nested() {
        panic!("implement Node::rebalance_recursive(): set all ratios to 0.5 recursively");
    }

    // Gap #4 (partial): layout template stores per-terminal metadata
    #[test]
    #[ignore = "gap #4 partial: LayoutTemplate doesn't store per-terminal cwd/command/group"]
    fn layout_template_with_metadata() {
        panic!("extend LayoutTemplate::Terminal to carry optional cwd, command, group, profile");
    }
}
