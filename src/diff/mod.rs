//! Recursive comparison of two directory trees by entry names, the collapsible outline
//! that shows it, and checks of file contents.

pub mod content;
mod outline;
#[cfg(test)]
mod tests;
mod walk;

use std::ffi::OsString;

pub use outline::Outline;
pub use walk::{Found, Progress, walk};

use crate::storage::{listing::EntryKind, location::Location};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Both,
    LeftOnly,
    RightOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }
}

impl Presence {
    pub fn is_on(self, side: Side) -> bool {
        self == Self::Both || self == Self::only(side)
    }

    /// Present on `side` and missing on the other.
    pub fn only(side: Side) -> Self {
        match side {
            Side::Left => Self::LeftOnly,
            Side::Right => Self::RightOnly,
        }
    }
}

/// An entry found on either side. A directory and a file with the same name are different
/// entries, each missing on one side.
#[derive(Debug)]
pub struct Node {
    pub name: String,
    /// Exact name on disk, for building paths.
    pub file_name: OsString,
    /// As found on the left side when the entry is on both.
    pub kind: EntryKind,
    pub presence: Presence,
    pub depth: usize,
    pub parent: Option<usize>,
    /// For a file on both sides, whether the two have the same contents; `None` for
    /// anything else.
    pub content: Option<Content>,
    /// For a file on both sides, its size on the right, as `kind` holds the left's.
    pub right_size: Option<u64>,
    /// One past the index of the last node in this node's subtree.
    pub end: usize,
    /// This node or something below it exists on one side only or has changed contents.
    pub differs: bool,
}

impl Node {
    /// Whether this node differs regardless of its children.
    fn differs_itself(&self) -> bool {
        self.presence != Presence::Both || self.content == Some(Content::Changed)
    }
}

/// How the contents of a file on both sides compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// Equal sizes; the bytes were not compared yet.
    Unchecked,
    Same,
    /// Different sizes or bytes.
    Changed,
}

impl Content {
    /// Equal sizes are only a hint, so they leave the contents unchecked.
    fn of(left: EntryKind, right: EntryKind) -> Option<Self> {
        match (left, right) {
            (EntryKind::File { size: left }, EntryKind::File { size: right }) => {
                Some(if left == right {
                    Self::Unchecked
                } else {
                    Self::Changed
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct ReadError {
    pub path: Location,
    pub message: String,
}

/// Union of both trees in pre-order, each directory's children in listing order.
/// Unreadable directories are compared as empty and reported in `errors`.
#[derive(Debug)]
pub struct TreeDiff {
    /// Compared directories, left then right.
    roots: [Location; 2],
    pub nodes: Vec<Node>,
    pub errors: Vec<ReadError>,
    /// Kept up to date as nodes are added, checked or copied, so reading it costs nothing.
    counts: Counts,
}

/// Differences in a [`TreeDiff`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub missing: Missing,
    /// Files on both sides with different contents.
    pub changed: usize,
    /// Files on both sides with equal sizes whose bytes were not compared yet.
    pub unchecked: usize,
}

/// Entries present on one side only, counting a one-sided directory once.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Missing {
    pub left: usize,
    pub right: usize,
}

impl Counts {
    /// Counts `node` as missing when it is one-sided and its parent, if any, is on both
    /// sides, and by its contents.
    fn add(&mut self, nodes: &[Node], node: usize) {
        let node = &nodes[node];
        if let Some(count) = self.of_content(node.content) {
            *count += 1;
        }
        if node
            .parent
            .is_some_and(|parent| nodes[parent].presence != Presence::Both)
        {
            return;
        }
        match node.presence {
            Presence::Both => {}
            Presence::LeftOnly => self.missing.right += 1,
            Presence::RightOnly => self.missing.left += 1,
        }
    }

    fn of_content(&mut self, content: Option<Content>) -> Option<&mut usize> {
        match content? {
            Content::Unchecked => Some(&mut self.unchecked),
            Content::Changed => Some(&mut self.changed),
            Content::Same => None,
        }
    }
}

impl TreeDiff {
    /// Comparison of `left` and `right` with nothing found yet.
    pub fn new(left: Location, right: Location) -> Self {
        Self {
            roots: [left, right],
            nodes: Vec::new(),
            errors: Vec::new(),
            counts: Counts::default(),
        }
    }

    /// Walks both trees fully.
    #[cfg(test)]
    pub fn compare(left: &Location, right: &Location) -> Self {
        let mut found = Vec::new();
        walk(left, right, &Progress::default(), &mut |item| {
            found.push(item)
        });
        let mut diff = Self::new(left.clone(), right.clone());
        diff.extend(found);
        diff
    }

    /// Adds what a [`walk`] found next, extending the subtrees and differences of the
    /// ancestors of new nodes. Returns the nodes, old or new, that started to differ.
    pub fn extend(&mut self, found: impl IntoIterator<Item = Found>) -> Vec<usize> {
        let mut started_differing = Vec::new();
        for item in found {
            let node = match item {
                Found::Node(node) => node,
                Found::Error(error) => {
                    self.errors.push(error);
                    continue;
                }
            };
            let index = self.nodes.len();
            debug_assert!(node.parent.is_none_or(|parent| parent < index));
            let differs = node.differs;
            let mut parent = node.parent;
            self.nodes.push(node);
            self.counts.add(&self.nodes, index);
            while let Some(index_above) = parent {
                let ancestor = &mut self.nodes[index_above];
                ancestor.end = index + 1;
                if differs && !ancestor.differs {
                    ancestor.differs = true;
                    started_differing.push(index_above);
                }
                parent = ancestor.parent;
            }
        }
        started_differing
    }

    pub fn has_children(&self, node: usize) -> bool {
        self.nodes[node].end > node + 1
    }

    pub fn root(&self, side: Side) -> &Location {
        &self.roots[side.index()]
    }

    /// Where `node` is, or would be, on `side`.
    pub fn location(&self, node: usize, side: Side) -> Location {
        let mut names = Vec::new();
        let mut current = Some(node);
        while let Some(index) = current {
            names.push(&self.nodes[index].file_name);
            current = self.nodes[index].parent;
        }
        names
            .into_iter()
            .rev()
            .fold(self.roots[side.index()].clone(), |location, name| {
                location.join(name)
            })
    }

    /// Records the verdict on the unchecked contents of `node`, a file on both sides.
    /// Returns the nodes that started to differ, `node` included.
    pub fn set_content(&mut self, node: usize, content: Content) -> Vec<usize> {
        let old = self.nodes[node].content.replace(content);
        debug_assert_eq!(old, Some(Content::Unchecked));
        if let Some(count) = self.counts.of_content(old) {
            *count -= 1;
        }
        if let Some(count) = self.counts.of_content(Some(content)) {
            *count += 1;
        }
        let mut started_differing = Vec::new();
        if content != Content::Changed {
            return started_differing;
        }
        // Above a node that already differs, everything does.
        let mut current = Some(node);
        while let Some(index) = current {
            let node = &mut self.nodes[index];
            if node.differs {
                break;
            }
            node.differs = true;
            started_differing.push(index);
            current = node.parent;
        }
        started_differing
    }

    /// Records that `nodes` now exist on both sides, files with the same contents, and
    /// updates which subtrees differ.
    pub fn mark_copied(&mut self, nodes: &[usize]) {
        for &node in nodes {
            let node = &mut self.nodes[node];
            node.presence = Presence::Both;
            (node.content, node.right_size) = match node.kind {
                EntryKind::File { size } => (Some(Content::Same), Some(size)),
                EntryKind::Directory => (None, None),
            };
        }
        self.recount();
    }

    /// Records that `nodes`, files on both sides, are gone from `side`, and updates which
    /// subtrees differ.
    pub fn mark_removed(&mut self, nodes: &[usize], side: Side) {
        for &node in nodes {
            let node = &mut self.nodes[node];
            debug_assert_eq!(node.presence, Presence::Both);
            node.presence = Presence::only(side.other());
            node.content = None;
            let right_size = node.right_size.take();
            // `kind` held the left's size; when the left is gone, the right's remains.
            if side == Side::Left
                && let (EntryKind::File { size }, Some(right)) = (&mut node.kind, right_size)
            {
                *size = right;
            }
        }
        self.recount();
    }

    /// Recomputes which subtrees differ and the counts after nodes changed in place.
    fn recount(&mut self) {
        // Children come after their parent, so a reverse pass sees every child first.
        let mut child_differs = vec![false; self.nodes.len()];
        for index in (0..self.nodes.len()).rev() {
            let node = &mut self.nodes[index];
            node.differs = node.differs_itself() || child_differs[index];
            if let Some(parent) = node.parent {
                child_differs[parent] |= node.differs;
            }
        }
        self.counts = Counts::default();
        for index in 0..self.nodes.len() {
            self.counts.add(&self.nodes, index);
        }
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }
}
