//! Which part of a [`TreeDiff`] is shown as rows.

use super::{Node, Presence, TreeDiff};

/// Which directories of a [`TreeDiff`] are expanded, whether entries equal on both sides
/// are hidden, and the nodes that are therefore visible.
#[derive(Debug)]
pub struct Outline {
    expansions: Vec<Expansion>,
    only_differences: bool,
    rows: Vec<usize>,
}

/// Whether a node is expanded and who decided it. The only automatic step is
/// `Untouched` → `AutoExpanded` when a directory present on both sides starts to differ;
/// toggling by the user moves any state to `UserExpanded` or `UserCollapsed`, which later
/// differences leave alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expansion {
    Untouched,
    AutoExpanded,
    UserExpanded,
    UserCollapsed,
}

impl Expansion {
    fn is_expanded(self) -> bool {
        match self {
            Self::AutoExpanded | Self::UserExpanded => true,
            Self::Untouched | Self::UserCollapsed => false,
        }
    }

    fn user(expanded: bool) -> Self {
        if expanded {
            Self::UserExpanded
        } else {
            Self::UserCollapsed
        }
    }

    /// The state after `node` started to differ.
    fn on_difference(self, node: &Node) -> Self {
        match self {
            Self::Untouched if node.presence == Presence::Both && node.kind.is_dir() => {
                Self::AutoExpanded
            }
            state => state,
        }
    }
}

impl Outline {
    /// Expands the directories present on both sides that differ, so every difference is
    /// visible except the contents of one-sided directories.
    pub fn new(diff: &TreeDiff, only_differences: bool) -> Self {
        let expansions = diff
            .nodes
            .iter()
            .map(|node| {
                if node.differs {
                    Expansion::Untouched.on_difference(node)
                } else {
                    Expansion::Untouched
                }
            })
            .collect();
        let mut outline = Self {
            expansions,
            only_differences,
            rows: Vec::new(),
        };
        outline.refresh(diff);
        outline
    }

    /// Takes in the nodes [`TreeDiff::extend`] added, untouched, and moves the nodes it
    /// returned in `started_differing` along; once the walk is done, the untouched part
    /// matches [`Self::new`].
    ///
    /// Rows of earlier nodes are kept: new nodes come last in pre-order, and an earlier
    /// node can only appear or show its children when it started to differ and then
    /// expanded or passed the differences filter. Rows are recomputed from the first such
    /// node, or from the first new node.
    pub fn grow(&mut self, diff: &TreeDiff, started_differing: &[usize]) {
        let added_from = self.expansions.len();
        self.expansions
            .resize(diff.nodes.len(), Expansion::Untouched);
        let mut changed_from = added_from;
        for &node in started_differing {
            let before = self.expansions[node];
            self.expansions[node] = before.on_difference(&diff.nodes[node]);
            if node < added_from && (self.expansions[node] != before || self.only_differences) {
                changed_from = changed_from.min(node);
            }
        }
        self.refresh_from(diff, changed_from);
    }

    pub fn only_differences(&self) -> bool {
        self.only_differences
    }

    /// Hides entries, with everything below them, that exist on both sides with equal
    /// contents.
    pub fn set_only_differences(&mut self, diff: &TreeDiff, only_differences: bool) {
        if self.only_differences != only_differences {
            self.only_differences = only_differences;
            self.refresh(diff);
        }
    }

    /// Row of `node`, or of the closest visible node above it when it is hidden.
    pub fn nearest_row(&self, node: usize) -> Option<usize> {
        let after = self.rows.partition_point(|&row_node| row_node <= node);
        after
            .checked_sub(1)
            .or((!self.rows.is_empty()).then_some(0))
    }

    /// Visible node indices, ascending.
    pub fn rows(&self) -> &[usize] {
        &self.rows
    }

    pub fn row_of(&self, node: usize) -> Option<usize> {
        self.rows.binary_search(&node).ok()
    }

    pub fn is_expanded(&self, node: usize) -> bool {
        self.expansions[node].is_expanded()
    }

    /// Expands or collapses `node` as the user's choice, which later differences keep.
    pub fn set_expanded(&mut self, diff: &TreeDiff, node: usize, expanded: bool) {
        let changed = self.is_expanded(node) != expanded;
        self.expansions[node] = Expansion::user(expanded);
        if changed {
            self.refresh(diff);
        }
    }

    /// Recomputes the visible rows, needed after the presence of nodes changed.
    pub fn refresh(&mut self, diff: &TreeDiff) {
        self.refresh_from(diff, 0);
    }

    /// Recomputes the rows of nodes from `start` on, keeping the ones before it.
    fn refresh_from(&mut self, diff: &TreeDiff, start: usize) {
        let kept = self.rows.partition_point(|&row| row < start);
        self.rows.truncate(kept);
        // Past the topmost ancestor hiding `start`, every step below stays on nodes whose
        // ancestors are all shown and expanded.
        let mut index = start;
        let mut ancestor = diff.nodes.get(start).and_then(|node| node.parent);
        while let Some(above) = ancestor {
            let node = &diff.nodes[above];
            if !self.is_expanded(above) || (self.only_differences && !node.differs) {
                index = node.end;
            }
            ancestor = node.parent;
        }
        while let Some(node) = diff.nodes.get(index) {
            if self.only_differences && !node.differs {
                index = node.end;
                continue;
            }
            self.rows.push(index);
            index = if self.is_expanded(index) {
                index + 1
            } else {
                node.end
            };
        }
    }
}
