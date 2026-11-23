//! Copying entries that exist on one side of a [`TreeDiff`] only to the other side.
//! Nothing is overwritten or deleted.

use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use anyhow::{Context as _, ensure};

use crate::{
    listing::EntryKind,
    location::Location,
    mtp,
    tree_diff::{Presence, Side, TreeDiff},
};

/// Creating one directory or copying one file.
#[derive(Debug)]
pub struct Op {
    pub node: usize,
    /// Directory that must be created first, when it is created by this plan too.
    parent: Option<usize>,
    pub kind: EntryKind,
    pub source: Location,
    pub target: Location,
}

impl Op {
    /// Data this operation transfers; creating a directory transfers none.
    fn bytes(&self) -> u64 {
        match self.kind {
            EntryKind::Directory => 0,
            EntryKind::File { size } => size,
        }
    }
}

#[derive(Debug)]
pub struct Plan {
    pub target: Side,
    /// Directories come before their contents.
    pub ops: Vec<Op>,
    pub files: usize,
    pub dirs: usize,
    pub bytes: u64,
}

impl Plan {
    /// Every entry within `scope` (the whole tree when `None`) that exists only on the side
    /// opposite to `target`.
    pub fn new(diff: &TreeDiff, scope: Option<usize>, target: Side) -> Self {
        let source = target.other();
        let mut plan = Self {
            target,
            ops: Vec::new(),
            files: 0,
            dirs: 0,
            bytes: 0,
        };
        for node in candidates(diff, scope, target) {
            let entry = &diff.nodes[node];
            match entry.kind {
                EntryKind::Directory => plan.dirs += 1,
                EntryKind::File { size } => {
                    plan.files += 1;
                    plan.bytes += size;
                }
            }
            plan.ops.push(Op {
                node,
                parent: entry
                    .parent
                    .filter(|&parent| diff.nodes[parent].presence != Presence::Both),
                kind: entry.kind,
                source: diff.location(node, source),
                target: diff.location(node, target),
            });
        }
        plan
    }
}

/// Whether [`Plan::new`] would find anything, without building the plan.
pub fn has_work(diff: &TreeDiff, scope: Option<usize>, target: Side) -> bool {
    candidates(diff, scope, target).next().is_some()
}

fn candidates(diff: &TreeDiff, scope: Option<usize>, target: Side) -> impl Iterator<Item = usize> {
    let range = scope.map_or(0..diff.nodes.len(), |node| node..diff.nodes[node].end);
    let source_only = Presence::only(target.other());
    range.filter(move |&node| diff.nodes[node].presence == source_only)
}

/// Bytes available to an unprivileged user at `location`.
pub fn free_space(location: &Location) -> anyhow::Result<u64> {
    match location {
        Location::Local(path) => {
            let stats = rustix::fs::statvfs(path)?;
            Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
        }
        Location::Mtp(path) => mtp::free_space(path),
    }
}

/// How far a running [`execute`] got, readable from other threads; also cancels it.
#[derive(Debug, Default)]
pub struct CopyProgress {
    bytes: AtomicU64,
    ops: AtomicUsize,
    current: Mutex<String>,
    cancelled: AtomicBool,
}

impl CopyProgress {
    /// Stops before the next entry; the one being copied finishes.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Bytes of files finished so far.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Operations finished so far, failed ones included.
    pub fn ops(&self) -> usize {
        self.ops.load(Ordering::Relaxed)
    }

    /// Name of the entry being copied.
    pub fn current(&self) -> String {
        self.current
            .lock()
            .map(|current| current.clone())
            .unwrap_or_default()
    }

    fn start(&self, name: String) {
        if let Ok(mut current) = self.current.lock() {
            *current = name;
        }
    }

    fn finish(&self, bytes: u64) {
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.ops.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub struct CopyError {
    pub target: Location,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Report {
    /// Nodes that now exist on the target side.
    pub copied: Vec<usize>,
    pub failed: Vec<CopyError>,
    /// Entries left out because their directory could not be created.
    pub skipped: usize,
    pub cancelled: bool,
}

/// Runs `plan` in order. A failure is recorded and the rest goes on, except for the
/// contents of a directory that could not be created.
pub fn execute(plan: &Plan, progress: &CopyProgress) -> Report {
    let mut report = Report::default();
    let mut not_created = HashSet::new();
    for op in &plan.ops {
        if progress.cancelled.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        if op
            .parent
            .is_some_and(|parent| not_created.contains(&parent))
        {
            not_created.insert(op.node);
            report.skipped += 1;
            progress.finish(op.bytes());
            continue;
        }
        progress.start(
            op.target
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        );
        let result = match op.kind {
            EntryKind::Directory => create_dir(&op.target),
            EntryKind::File { .. } => copy_file(&op.source, &op.target),
        };
        match result {
            Ok(()) => report.copied.push(op.node),
            Err(error) => {
                tracing::warn!(source = %op.source, target = %op.target, error = format!("{error:#}"), "cannot copy");
                not_created.insert(op.node);
                report.failed.push(CopyError {
                    target: op.target.clone(),
                    message: format!("{error:#}"),
                });
            }
        }
        progress.finish(op.bytes());
    }
    report
}

fn create_dir(target: &Location) -> anyhow::Result<()> {
    match target {
        Location::Local(path) => Ok(fs::create_dir(path)?),
        Location::Mtp(path) => mtp::create_dir(path),
    }
}

fn copy_file(source: &Location, target: &Location) -> anyhow::Result<()> {
    match (source, target) {
        (_, Location::Local(target)) => copy_to_local(source, target),
        (Location::Local(source), Location::Mtp(target)) => {
            mtp::send_file(&File::open(source)?, target)
        }
        (Location::Mtp(source), Location::Mtp(target)) => {
            let buffer = tempfile::tempfile()?;
            mtp::receive_file(source, &buffer)?;
            mtp::send_file(&buffer, target)
        }
    }
}

/// Writes next to `target` under a hidden name and renames it at the end, so an
/// interrupted copy never looks like a complete file.
fn copy_to_local(source: &Location, target: &Path) -> anyhow::Result<()> {
    ensure!(!fs::exists(target)?, "already exists");
    let part = part_path(target)?;
    let result = write_local(source, &part).and_then(|()| Ok(fs::rename(&part, target)?));
    if result.is_err() {
        _ = fs::remove_file(&part);
    }
    result
}

fn write_local(source: &Location, part: &Path) -> anyhow::Result<()> {
    match source {
        Location::Local(source) => {
            fs::copy(source, part)?;
        }
        Location::Mtp(source) => {
            let file = File::create(part)?;
            mtp::receive_file(source, &file)?;
            file.sync_all()?;
        }
    }
    Ok(())
}

fn part_path(target: &Path) -> anyhow::Result<PathBuf> {
    let name = target.file_name().context("target has no file name")?;
    let mut part = OsString::from(".");
    part.push(name);
    part.push(".music-syncer-part");
    Ok(target.with_file_name(part))
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    fn build(root: &Path, entries: &[&str]) -> io::Result<()> {
        for entry in entries {
            let path = root.join(entry.trim_end_matches('/'));
            if entry.ends_with('/') {
                fs::create_dir_all(path)?;
            } else {
                fs::create_dir_all(path.parent().unwrap_or(root))?;
                fs::write(path, entry.as_bytes())?;
            }
        }
        Ok(())
    }

    fn compare(left: &Path, right: &Path) -> TreeDiff {
        TreeDiff::compare(
            &Location::local(left.to_path_buf()),
            &Location::local(right.to_path_buf()),
        )
    }

    #[test]
    fn copies_everything_missing_on_the_right_and_marks_it() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let (left, right) = (temp.path().join("left"), temp.path().join("right"));
        build(
            &left,
            &[
                "Album/CD1/01.flac",
                "Album/CD1/02.flac",
                "New/Deep/x.mp3",
                "solo.mp3",
            ],
        )?;
        build(&right, &["Album/CD1/01.flac", "only-right.mp3"])?;
        let mut diff = compare(&left, &right);

        let plan = Plan::new(&diff, None, Side::Right);
        let targets = plan
            .ops
            .iter()
            .map(|op| op.target.to_string())
            .collect::<Vec<_>>();
        let at = |path: &str| right.join(path).display().to_string();
        assert_eq!(
            targets,
            [
                at("Album/CD1/02.flac"),
                at("New"),
                at("New/Deep"),
                at("New/Deep/x.mp3"),
                at("solo.mp3"),
            ]
        );
        assert_eq!((plan.files, plan.dirs), (3, 2));
        assert_eq!(
            plan.bytes,
            ["Album/CD1/02.flac", "New/Deep/x.mp3", "solo.mp3"]
                .iter()
                .map(|name| name.len() as u64)
                .sum::<u64>()
        );

        let report = execute(&plan, &CopyProgress::default());

        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(
            fs::read_to_string(right.join("New/Deep/x.mp3"))?,
            "New/Deep/x.mp3"
        );
        diff.mark_on_both(&report.copied);
        let rescanned = compare(&left, &right);
        assert_eq!(diff.missing(), rescanned.missing());
        assert_eq!(rescanned.missing().right, 0);
        assert_eq!(rescanned.missing().left, 1);
        Ok(())
    }

    #[test]
    fn contents_of_a_directory_that_cannot_be_created_are_skipped() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let (left, right) = (temp.path().join("left"), temp.path().join("right"));
        build(&left, &["Blocked/a.mp3", "Blocked/b.mp3", "fine.mp3"])?;
        // A file named like the directory makes creating it fail.
        build(&right, &["Blocked"])?;
        let diff = compare(&left, &right);

        let report = execute(
            &Plan::new(&diff, None, Side::Right),
            &CopyProgress::default(),
        );

        assert_eq!(report.failed.len(), 1);
        assert_eq!(
            report.failed[0].target,
            Location::Local(right.join("Blocked"))
        );
        assert_eq!(report.skipped, 2);
        let copied = report
            .copied
            .iter()
            .map(|&node| diff.nodes[node].name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(copied, ["fine.mp3"]);
        Ok(())
    }

    #[test]
    fn plan_for_a_directory_on_both_sides_covers_only_its_contents() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let (left, right) = (temp.path().join("left"), temp.path().join("right"));
        build(&left, &["A/1.mp3", "B/2.mp3"])?;
        build(&right, &["A/", "B/", "A/3.mp3"])?;
        let diff = compare(&left, &right);
        let a = diff.nodes.iter().position(|node| node.name == "A");

        let to_right = a.map(|a| Plan::new(&diff, Some(a), Side::Right));
        let to_left = a.map(|a| Plan::new(&diff, Some(a), Side::Left));

        let names = |plan: Option<Plan>| {
            plan.map(|plan| {
                plan.ops
                    .iter()
                    .map(|op| diff.nodes[op.node].name.clone())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(names(to_right), Some(vec!["1.mp3".to_owned()]));
        assert_eq!(names(to_left), Some(vec!["3.mp3".to_owned()]));
        Ok(())
    }
}
