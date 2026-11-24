//! Reading both trees, merging their entries into [`TreeDiff`](super::TreeDiff) nodes.

use std::{
    collections::BTreeMap,
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use super::{Content, Node, Presence, ReadError, Side};
use crate::storage::{
    listing::{self, Entry, EntryKind, SortKey},
    location::Location,
};

/// How far a running [`walk`] got, readable from other threads.
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
/// lead back to a directory being compared above them.
/// [`TreeDiff::extend`](super::TreeDiff::extend) collects the results.
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
