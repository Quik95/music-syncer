//! Recursive comparison of two directory trees by entry names, and the collapsible outline
//! that shows it.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use crate::{
    listing::{self, Entry, EntryKind, SortKey},
    location::Location,
};

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

/// How far a running [`TreeDiff::compare`] got, readable from other threads.
#[derive(Debug, Default)]
pub struct Progress {
    /// Estimated fraction done, in millionths.
    done: AtomicU32,
    entries: AtomicUsize,
}

impl Progress {
    const SCALE: f64 = 1_000_000.;

    /// Estimated share of the walk done, 0–1. Each directory's children split its share
    /// equally, as sizes of unread subtrees are unknown.
    pub fn fraction(&self) -> f32 {
        (f64::from(self.done.load(Ordering::Relaxed)) / Self::SCALE) as f32
    }

    /// Entries found so far on either side.
    pub fn entries(&self) -> usize {
        self.entries.load(Ordering::Relaxed)
    }

    /// `levels` holds, from the root down, the index of the child being walked and the
    /// number of children at that level.
    fn update(&self, levels: &[(usize, usize)], entries: usize) {
        let mut done = 0.;
        let mut share = 1.;
        for &(index, count) in levels {
            share /= count as f64;
            done += share * index as f64;
        }
        // Saturating float-to-int cast of a value within 0..=SCALE.
        self.done
            .store((done * Self::SCALE) as u32, Ordering::Relaxed);
        self.entries.store(entries, Ordering::Relaxed);
    }
}

/// Something a running [`walk`] found, in the order it found it.
#[derive(Debug)]
pub enum Found {
    /// The next node in pre-order; its subtree follows.
    Node(Node),
    Error(ReadError),
}

/// Reads both trees fully, passing every node and error to `found` as soon as it is
/// known and reporting to `progress`; local symlinked directories are followed unless they
/// lead back to a directory being compared above them. [`TreeDiff::extend`] collects the
/// results.
pub fn walk(left: &Location, right: &Location, progress: &Progress, found: &mut dyn FnMut(Found)) {
    let mut builder = Builder {
        found,
        count: 0,
        levels: Vec::new(),
        progress,
    };
    let mut ancestors = [Vec::new(), Vec::new()];
    let roots = [left, right].map(|root| Some(root.clone()));
    let roots = builder.enter(roots, &mut ancestors);
    builder.add_children(&roots, None, 0, &mut ancestors);
    progress.update(&[(1, 1)], builder.count);
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

struct Builder<'a> {
    found: &'a mut dyn FnMut(Found),
    /// Nodes found so far; the next one gets this index.
    count: usize,
    /// Position in each directory being walked, root first; see [`Progress::update`].
    levels: Vec<(usize, usize)>,
    progress: &'a Progress,
}

impl Builder<'_> {
    /// Children of `dirs`, merged by name.
    fn add_children(
        &mut self,
        dirs: &[Option<Location>; 2],
        parent: Option<usize>,
        depth: usize,
        ancestors: &mut [Vec<PathBuf>; 2],
    ) {
        let mut merged = BTreeMap::<SortKey, [Option<Entry>; 2]>::new();
        for (side, dir) in dirs.iter().enumerate() {
            let Some(dir) = dir else { continue };
            match listing::read_entries(dir) {
                Ok(entries) => {
                    for entry in entries {
                        let key = listing::sort_key(&entry);
                        merged.entry(key).or_default()[side] = Some(entry);
                    }
                }
                Err(error) => self.error(dir, &error),
            }
        }

        self.levels.push((0, merged.len()));
        for (position, pair) in merged.into_values().enumerate() {
            if let Some(level) = self.levels.last_mut() {
                level.0 = position;
            }
            self.progress.update(&self.levels, self.count);
            let (presence, content, right_size, entry) = match pair {
                [Some(entry), Some(right)] => {
                    let right_size = match right.kind {
                        EntryKind::File { size } => Some(size),
                        EntryKind::Directory => None,
                    };
                    let content = Content::of(entry.kind, right.kind);
                    (Presence::Both, content, right_size, entry)
                }
                [Some(entry), None] => (Presence::LeftOnly, None, None, entry),
                [None, Some(entry)] => (Presence::RightOnly, None, None, entry),
                [None, None] => unreachable!("every merged key has an entry"),
            };
            let index = self.count;
            self.count += 1;
            let mut node = Node {
                name: entry.name,
                file_name: entry.file_name.clone(),
                kind: entry.kind,
                presence,
                depth,
                parent,
                content,
                right_size,
                end: index + 1,
                differs: false,
            };
            node.differs = node.differs_itself();
            (self.found)(Found::Node(node));
            if entry.kind.is_dir() {
                let present = [Side::Left, Side::Right].map(|side| presence.is_on(side));
                let children = std::array::from_fn(|side| {
                    dirs[side]
                        .as_ref()
                        .filter(|_| present[side])
                        .map(|dir| dir.join(&entry.file_name))
                });
                let children = self.enter(children, ancestors);
                self.add_children(&children, Some(index), depth + 1, ancestors);
                for (side, child) in children.iter().enumerate() {
                    if child
                        .as_ref()
                        .is_some_and(|child| child.as_local().is_some())
                    {
                        ancestors[side].pop();
                    }
                }
            }
        }
        self.levels.pop();
    }

    /// Records each local directory as an ancestor on its side. A local directory that
    /// cannot be resolved or is already an ancestor is reported and not descended into.
    /// MTP has no symlinks, so its directories pass unchecked.
    fn enter(
        &mut self,
        dirs: [Option<Location>; 2],
        ancestors: &mut [Vec<PathBuf>; 2],
    ) -> [Option<Location>; 2] {
        let mut side = 0;
        dirs.map(|dir| {
            let ancestors = &mut ancestors[side];
            side += 1;
            let dir = dir?;
            let Some(local) = dir.as_local() else {
                return Some(dir);
            };
            match fs::canonicalize(local) {
                Ok(real) if ancestors.contains(&real) => {
                    tracing::warn!(path = %dir, target = %real.display(), "symlink loop");
                    (self.found)(Found::Error(ReadError {
                        message: format!("symlink loop back to {}", real.display()),
                        path: dir,
                    }));
                    None
                }
                Ok(real) => {
                    ancestors.push(real);
                    Some(dir)
                }
                Err(error) => {
                    self.error(&dir, &error);
                    None
                }
            }
        })
    }

    fn error(&mut self, path: &Location, error: &io::Error) {
        tracing::warn!(%path, %error, "cannot compare directory");
        (self.found)(Found::Error(ReadError {
            path: path.clone(),
            message: error.to_string(),
        }));
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn compare(temp: &tempfile::TempDir, left: &str, right: &str) -> TreeDiff {
        let side = |name| Location::local(temp.path().join(name));
        TreeDiff::compare(&side(left), &side(right))
    }

    fn fixture() -> io::Result<tempfile::TempDir> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        for (path, is_dir) in [
            ("left/Album/CD1", true),
            ("left/Album/CD1/01.flac", false),
            ("left/Album/CD1/02.flac", false),
            ("left/Same", true),
            ("left/Same/a.mp3", false),
            ("left/Gone", true),
            ("left/Gone/x.mp3", false),
            ("left/cover", false),
            ("right/Album/CD1", true),
            ("right/Album/CD1/01.flac", false),
            ("right/Same", true),
            ("right/Same/a.mp3", false),
            ("right/cover", true),
            ("right/10.flac", false),
            ("right/9.flac", false),
        ] {
            if is_dir {
                fs::create_dir_all(root.join(path))?;
            } else {
                fs::write(root.join(path), "")?;
            }
        }
        Ok(temp)
    }

    #[test]
    fn merges_both_trees_and_marks_one_sided_entries() -> io::Result<()> {
        let temp = fixture()?;
        let diff = compare(&temp, "left", "right");

        let nodes = diff
            .nodes
            .iter()
            .map(|node| (node.depth, node.name.as_str(), node.presence, node.differs))
            .collect::<Vec<_>>();
        use Presence::*;
        assert_eq!(
            nodes,
            [
                (0, "Album", Both, true),
                (1, "CD1", Both, true),
                (2, "01.flac", Both, false),
                (2, "02.flac", LeftOnly, true),
                (0, "cover", RightOnly, true),
                (0, "Gone", LeftOnly, true),
                (1, "x.mp3", LeftOnly, true),
                (0, "Same", Both, false),
                (1, "a.mp3", Both, false),
                (0, "9.flac", RightOnly, true),
                (0, "10.flac", RightOnly, true),
                (0, "cover", LeftOnly, true),
            ]
        );
        assert_eq!(
            diff.counts(),
            Counts {
                missing: Missing { left: 3, right: 3 },
                changed: 0,
                unchecked: 2,
            }
        );
        assert!(diff.errors.is_empty());
        Ok(())
    }

    #[test]
    fn outline_expands_only_differing_directories_on_both_sides() -> io::Result<()> {
        let temp = fixture()?;
        let diff = compare(&temp, "left", "right");
        let mut outline = Outline::new(&diff, false);

        let names = |outline: &Outline| {
            outline
                .rows()
                .iter()
                .map(|&node| diff.nodes[node].name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&outline),
            [
                "Album", "CD1", "01.flac", "02.flac", "cover", "Gone", "Same", "9.flac", "10.flac",
                "cover"
            ]
        );

        outline.set_expanded(&diff, 0, false);
        assert_eq!(
            names(&outline),
            [
                "Album", "cover", "Gone", "Same", "9.flac", "10.flac", "cover"
            ]
        );
        assert_eq!(outline.row_of(7), Some(3));
        assert_eq!(outline.row_of(8), None);

        outline.set_only_differences(&diff, true);
        assert_eq!(
            names(&outline),
            ["Album", "cover", "Gone", "9.flac", "10.flac", "cover"]
        );
        // "Same" is hidden; the nearest row above it is "Gone".
        assert_eq!(outline.nearest_row(7), Some(2));

        outline.set_expanded(&diff, 0, true);
        assert_eq!(
            names(&outline),
            [
                "Album", "CD1", "02.flac", "cover", "Gone", "9.flac", "10.flac", "cover"
            ]
        );
        // "01.flac" is equal on both sides, so "CD1" stands in for it.
        assert_eq!(outline.nearest_row(2), Some(1));
        Ok(())
    }

    #[test]
    fn growing_one_node_at_a_time_ends_like_the_whole_comparison() -> io::Result<()> {
        let temp = fixture()?;
        let whole = compare(&temp, "left", "right");
        let side = |name| Location::local(temp.path().join(name));
        let shape = |diff: &TreeDiff| {
            diff.nodes
                .iter()
                .map(|node| (node.end, node.differs))
                .collect::<Vec<_>>()
        };

        for only_differences in [false, true] {
            let mut found = Vec::new();
            walk(
                &side("left"),
                &side("right"),
                &Progress::default(),
                &mut |item| {
                    found.push(item);
                },
            );
            let mut diff = TreeDiff::new(side("left"), side("right"));
            let mut outline = Outline::new(&diff, only_differences);

            for item in found {
                let started_differing = diff.extend([item]);
                outline.grow(&diff, &started_differing);
                // Every intermediate state matches a full recomputation.
                let rows = outline.rows().to_vec();
                outline.refresh(&diff);
                assert_eq!(rows, outline.rows());
            }

            assert_eq!(shape(&diff), shape(&whole));
            assert_eq!(
                outline.rows(),
                Outline::new(&whole, only_differences).rows()
            );
            assert_eq!(
                diff.counts(),
                Counts {
                    missing: Missing { left: 3, right: 3 },
                    changed: 0,
                    unchecked: 2,
                }
            );
        }
        Ok(())
    }

    #[test]
    fn later_difference_keeps_a_directory_the_user_collapsed() -> io::Result<()> {
        let temp = fixture()?;
        let side = |name| Location::local(temp.path().join(name));
        let mut found = Vec::new();
        walk(
            &side("left"),
            &side("right"),
            &Progress::default(),
            &mut |item| {
                found.push(item);
            },
        );
        let mut diff = TreeDiff::new(side("left"), side("right"));
        let mut outline = Outline::new(&diff, false);
        let mut found = found.into_iter();

        // "Album", "CD1" and "01.flac" match so far.
        let started_differing = diff.extend(found.by_ref().take(3));
        outline.grow(&diff, &started_differing);
        assert!(!diff.nodes[0].differs);
        outline.set_expanded(&diff, 0, true);
        outline.set_expanded(&diff, 0, false);

        // "02.flac" makes "Album" and the untouched "CD1" differ.
        let started_differing = diff.extend(found);
        outline.grow(&diff, &started_differing);

        assert!(diff.nodes[0].differs);
        assert!(!outline.is_expanded(0));
        assert!(outline.is_expanded(1));
        Ok(())
    }

    #[test]
    fn files_of_different_sizes_pair_up_as_changed() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        for (side, contents) in [("left", "short"), ("right", "much longer")] {
            fs::create_dir_all(temp.path().join(side).join("Album"))?;
            fs::write(temp.path().join(side).join("Album/song.mp3"), contents)?;
            fs::write(temp.path().join(side).join("Album/same-size.mp3"), "x")?;
        }

        let diff = compare(&temp, "left", "right");

        let nodes = diff
            .nodes
            .iter()
            .map(|node| {
                (
                    node.name.as_str(),
                    node.presence,
                    node.content,
                    node.differs,
                )
            })
            .collect::<Vec<_>>();
        use Presence::Both;
        assert_eq!(
            nodes,
            [
                ("Album", Both, None, true),
                ("same-size.mp3", Both, Some(Content::Unchecked), false),
                ("song.mp3", Both, Some(Content::Changed), true),
            ]
        );
        assert_eq!((diff.counts().changed, diff.counts().unchecked), (1, 1));
        Ok(())
    }

    #[test]
    fn file_removed_from_the_right_becomes_missing_there() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        for (side, contents) in [("left", "new version"), ("right", "old")] {
            fs::create_dir_all(temp.path().join(side).join("Album"))?;
            fs::write(temp.path().join(side).join("Album/song.mp3"), contents)?;
        }
        let mut diff = compare(&temp, "left", "right");
        assert_eq!((diff.counts().changed, diff.counts().missing.right), (1, 0));

        diff.mark_removed(&[1], Side::Right);

        let song = &diff.nodes[1];
        assert_eq!((song.presence, song.content), (Presence::LeftOnly, None));
        assert!(diff.nodes[0].differs);
        assert_eq!((diff.counts().changed, diff.counts().missing.right), (0, 1));
        Ok(())
    }

    #[test]
    fn changed_contents_mark_ancestors_until_one_already_differs() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        for side in ["left", "right"] {
            fs::create_dir_all(temp.path().join(side).join("A/B"))?;
            fs::write(temp.path().join(side).join("A/B/x.mp3"), "same size")?;
            fs::write(temp.path().join(side).join("A/y.mp3"), "same size")?;
        }
        let mut diff = compare(&temp, "left", "right");
        let mut outline = Outline::new(&diff, true);
        assert!(outline.rows().is_empty());
        let index = |name: &str| diff.nodes.iter().position(|node| node.name == name);
        let (Some(x), Some(y)) = (index("x.mp3"), index("y.mp3")) else {
            panic!("fixture lacks files");
        };

        let started = diff.set_content(x, Content::Changed);
        outline.grow(&diff, &started);

        assert_eq!(started, [x, x - 1, x - 2]);
        assert_eq!(outline.rows(), [x - 2, x - 1, x]);
        // "A" already differs, so only "y.mp3" itself starts to.
        assert_eq!(diff.set_content(y, Content::Changed), [y]);
        assert_eq!((diff.counts().changed, diff.counts().unchecked), (2, 0));
        Ok(())
    }

    #[test]
    fn unreadable_side_is_reported_and_compared_as_empty() -> io::Result<()> {
        let temp = fixture()?;
        let diff = compare(&temp, "left", "missing");

        assert_eq!(diff.errors.len(), 1);
        assert_eq!(
            diff.errors[0].path,
            Location::Local(temp.path().join("missing"))
        );
        assert!(
            diff.nodes
                .iter()
                .all(|node| node.presence == Presence::LeftOnly)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loop_is_not_followed() -> io::Result<()> {
        let temp = fixture()?;
        let left = temp.path().join("left");
        std::os::unix::fs::symlink(&left, left.join("Same/loop"))?;

        let diff = compare(&temp, "left", "right");

        assert_eq!(diff.errors.len(), 1);
        assert_eq!(diff.errors[0].path, Location::Local(left.join("Same/loop")));
        let looped = diff.nodes.iter().position(|node| node.name == "loop");
        assert!(looped.is_some_and(|index| !diff.has_children(index)));
        Ok(())
    }
}
