//! Recursive comparison of two directory trees by entry names, and the collapsible outline
//! that shows it.

use std::{
    collections::BTreeMap,
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

impl Presence {
    pub fn on_left(self) -> bool {
        self != Self::RightOnly
    }

    pub fn on_right(self) -> bool {
        self != Self::LeftOnly
    }
}

/// An entry found on either side. A directory and a file with the same name are different
/// entries, each missing on one side.
#[derive(Debug)]
pub struct Node {
    pub name: String,
    pub kind: EntryKind,
    pub presence: Presence,
    pub depth: usize,
    pub parent: Option<usize>,
    /// One past the index of the last node in this node's subtree.
    pub end: usize,
    /// This node or something below it exists on one side only.
    pub differs: bool,
}

impl Node {
    pub fn has_children(&self, index: usize) -> bool {
        self.end > index + 1
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
    pub nodes: Vec<Node>,
    pub errors: Vec<ReadError>,
}

/// Entries present on one side only, counting a one-sided directory once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Missing {
    pub left: usize,
    pub right: usize,
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

impl TreeDiff {
    /// Reads both trees fully, reporting to `progress`; local symlinked directories are
    /// followed unless they lead back to a directory being compared above them.
    pub fn compare(left: &Location, right: &Location, progress: &Progress) -> Self {
        let mut builder = Builder {
            nodes: Vec::new(),
            errors: Vec::new(),
            levels: Vec::new(),
            progress,
        };
        let mut ancestors = [Vec::new(), Vec::new()];
        let roots = [left, right].map(|root| Some(root.clone()));
        let roots = builder.enter(roots, &mut ancestors);
        builder.add_children(&roots, None, 0, &mut ancestors);
        progress.update(&[(1, 1)], builder.nodes.len());
        Self {
            nodes: builder.nodes,
            errors: builder.errors,
        }
    }

    pub fn missing(&self) -> Missing {
        let mut missing = Missing { left: 0, right: 0 };
        let topmost = self.nodes.iter().filter(|node| {
            node.parent
                .is_none_or(|parent| self.nodes[parent].presence == Presence::Both)
        });
        for node in topmost {
            match node.presence {
                Presence::Both => {}
                Presence::LeftOnly => missing.right += 1,
                Presence::RightOnly => missing.left += 1,
            }
        }
        missing
    }
}

struct Builder<'a> {
    nodes: Vec<Node>,
    errors: Vec<ReadError>,
    /// Position in each directory being walked, root first; see [`Progress::update`].
    levels: Vec<(usize, usize)>,
    progress: &'a Progress,
}

impl Builder<'_> {
    /// Children of `dirs`, merged by name. Returns whether any of them differs.
    fn add_children(
        &mut self,
        dirs: &[Option<Location>; 2],
        parent: Option<usize>,
        depth: usize,
        ancestors: &mut [Vec<PathBuf>; 2],
    ) -> bool {
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

        let mut differs = false;
        self.levels.push((0, merged.len()));
        for (position, pair) in merged.into_values().enumerate() {
            if let Some(level) = self.levels.last_mut() {
                level.0 = position;
            }
            self.progress.update(&self.levels, self.nodes.len());
            let (presence, entry) = match pair {
                [Some(entry), Some(_)] => (Presence::Both, entry),
                [Some(entry), None] => (Presence::LeftOnly, entry),
                [None, Some(entry)] => (Presence::RightOnly, entry),
                [None, None] => unreachable!("every merged key has an entry"),
            };
            let index = self.nodes.len();
            self.nodes.push(Node {
                name: entry.name,
                kind: entry.kind,
                presence,
                depth,
                parent,
                end: index + 1,
                differs: presence != Presence::Both,
            });
            if entry.kind == EntryKind::Directory {
                let present = [presence.on_left(), presence.on_right()];
                let children = std::array::from_fn(|side| {
                    dirs[side]
                        .as_ref()
                        .filter(|_| present[side])
                        .map(|dir| dir.join(&entry.file_name))
                });
                let children = self.enter(children, ancestors);
                let child_differs = self.add_children(&children, Some(index), depth + 1, ancestors);
                for (side, child) in children.iter().enumerate() {
                    if child
                        .as_ref()
                        .is_some_and(|child| child.as_local().is_some())
                    {
                        ancestors[side].pop();
                    }
                }
                let end = self.nodes.len();
                let node = &mut self.nodes[index];
                node.end = end;
                node.differs |= child_differs;
            }
            differs |= self.nodes[index].differs;
        }
        self.levels.pop();
        differs
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
                    self.errors.push(ReadError {
                        message: format!("symlink loop back to {}", real.display()),
                        path: dir,
                    });
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
        self.errors.push(ReadError {
            path: path.clone(),
            message: error.to_string(),
        });
    }
}

/// Which directories of a [`TreeDiff`] are expanded, and the nodes that are therefore visible.
#[derive(Debug)]
pub struct Outline {
    expanded: Vec<bool>,
    rows: Vec<usize>,
}

impl Outline {
    /// Expands the directories present on both sides that differ, so every difference is
    /// visible except the contents of one-sided directories.
    pub fn new(diff: &TreeDiff) -> Self {
        let expanded = diff
            .nodes
            .iter()
            .map(|node| node.presence == Presence::Both && node.differs)
            .collect();
        let mut outline = Self {
            expanded,
            rows: Vec::new(),
        };
        outline.update_rows(diff);
        outline
    }

    /// Visible node indices, ascending.
    pub fn rows(&self) -> &[usize] {
        &self.rows
    }

    pub fn row_of(&self, node: usize) -> Option<usize> {
        self.rows.binary_search(&node).ok()
    }

    pub fn is_expanded(&self, node: usize) -> bool {
        self.expanded[node]
    }

    pub fn set_expanded(&mut self, diff: &TreeDiff, node: usize, expanded: bool) {
        if self.expanded[node] != expanded {
            self.expanded[node] = expanded;
            self.update_rows(diff);
        }
    }

    fn update_rows(&mut self, diff: &TreeDiff) {
        self.rows.clear();
        let mut index = 0;
        while let Some(node) = diff.nodes.get(index) {
            self.rows.push(index);
            index = if self.expanded[index] {
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
        TreeDiff::compare(&side(left), &side(right), &Progress::default())
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
        assert_eq!(diff.missing(), Missing { left: 3, right: 3 });
        assert!(diff.errors.is_empty());
        Ok(())
    }

    #[test]
    fn outline_expands_only_differing_directories_on_both_sides() -> io::Result<()> {
        let temp = fixture()?;
        let diff = compare(&temp, "left", "right");
        let mut outline = Outline::new(&diff);

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
        assert!(looped.is_some_and(|index| !diff.nodes[index].has_children(index)));
        Ok(())
    }
}
