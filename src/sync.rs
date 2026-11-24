//! Making the sides of a [`TreeDiff`] match: copying entries one side lacks, and replacing
//! changed files on the right with the left's version. Nothing is deleted.

use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, ensure};

use crate::{
    diff::{Content, Node, Presence, Side, TreeDiff},
    progress::JobProgress,
    storage::{
        listing::EntryKind,
        location::{Location, MtpPath},
        mtp,
    },
};

/// What a [`Plan`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Creates the entries `target` lacks from the other side; overwrites nothing.
    Copy { target: Side },
    /// Replaces files on the right whose contents differ with the left's version.
    Overwrite,
}

impl Action {
    pub fn target(self) -> Side {
        match self {
            Self::Copy { target } => target,
            Self::Overwrite => Side::Right,
        }
    }

    fn includes(self, node: &Node) -> bool {
        match self {
            Self::Copy { target } => node.presence == Presence::only(target.other()),
            Self::Overwrite => node.content == Some(Content::Changed),
        }
    }
}

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
    pub action: Action,
    /// Directories come before their contents.
    pub ops: Vec<Op>,
    pub files: usize,
    pub dirs: usize,
    /// Data written to the target.
    pub bytes: u64,
    /// Most free space the target needs at any point, as files are replaced one by one.
    pub space: u64,
}

impl Plan {
    /// Every entry within `scope` (the whole tree when `None`) that `action` applies to.
    pub fn new(diff: &TreeDiff, action: Action, scope: Option<usize>) -> Self {
        let target = action.target();
        let source = target.other();
        // A local file is written in full before the version it replaces goes away; on MTP
        // the old version is deleted first.
        let keeps_old_while_writing = diff.root(target).as_local().is_some();
        let mut plan = Self {
            action,
            ops: Vec::new(),
            files: 0,
            dirs: 0,
            bytes: 0,
            space: 0,
        };
        // Growth of the target so far; negative when replaced files shrank.
        let mut grown = 0_i128;
        let mut peak = 0_i128;
        for node in candidates(diff, action, scope) {
            let entry = &diff.nodes[node];
            match entry.kind {
                EntryKind::Directory => plan.dirs += 1,
                EntryKind::File { size } => {
                    plan.files += 1;
                    plan.bytes += size;
                    let replaced = match action {
                        Action::Copy { .. } => 0,
                        Action::Overwrite => entry.right_size.unwrap_or(0),
                    };
                    let (size, replaced) = (i128::from(size), i128::from(replaced));
                    let during = if keeps_old_while_writing {
                        grown + size
                    } else {
                        grown + size - replaced
                    };
                    peak = peak.max(during);
                    grown += size - replaced;
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
        // Never negative, as it starts at 0, and at most `bytes`.
        plan.space = u64::try_from(peak).unwrap_or(0);
        plan
    }
}

/// Whether [`Plan::new`] would find anything, without building the plan: from the counts
/// for the whole tree, else by scanning the subtree unless nothing in it differs.
pub fn has_work(diff: &TreeDiff, action: Action, scope: Option<usize>) -> bool {
    let Some(node) = scope else {
        let counts = diff.counts();
        return match action {
            Action::Copy {
                target: Side::Right,
            } => counts.missing.right > 0,
            Action::Copy { target: Side::Left } => counts.missing.left > 0,
            Action::Overwrite => counts.changed > 0,
        };
    };
    diff.nodes[node].differs && candidates(diff, action, scope).next().is_some()
}

fn candidates(
    diff: &TreeDiff,
    action: Action,
    scope: Option<usize>,
) -> impl Iterator<Item = usize> {
    let range = scope.map_or(0..diff.nodes.len(), |node| node..diff.nodes[node].end);
    range.filter(move |&node| action.includes(&diff.nodes[node]))
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

#[derive(Debug)]
pub struct CopyError {
    pub target: Location,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Report {
    /// Nodes that now exist on both sides, files with the same contents.
    pub copied: Vec<usize>,
    pub failed: Vec<CopyError>,
    /// Failed overwrites whose old version is gone too, now missing on the target.
    pub removed: Vec<usize>,
    /// Entries left out because their directory could not be created.
    pub skipped: usize,
    pub cancelled: bool,
}

/// Runs `plan` in order. A failure is recorded and the rest goes on, except for the
/// contents of a directory that could not be created.
pub fn execute(plan: &Plan, progress: &JobProgress) -> Report {
    let replace = plan.action == Action::Overwrite;
    let mut report = Report::default();
    let mut not_created = HashSet::new();
    for op in &plan.ops {
        if progress.is_cancelled() {
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
            EntryKind::File { .. } => copy_file(&op.source, &op.target, replace),
        };
        match result {
            Ok(()) => report.copied.push(op.node),
            Err(error) => {
                tracing::warn!(source = %op.source, target = %op.target, error = format!("{error:#}"), "cannot copy");
                not_created.insert(op.node);
                if replace && exists(&op.target).is_ok_and(|exists| !exists) {
                    report.removed.push(op.node);
                }
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

fn exists(location: &Location) -> anyhow::Result<bool> {
    match location {
        Location::Local(path) => Ok(fs::exists(path)?),
        Location::Mtp(path) => mtp::exists(path),
    }
}

fn create_dir(target: &Location) -> anyhow::Result<()> {
    match target {
        Location::Local(path) => {
            fs::create_dir(path)?;
            sync_parent(path)
        }
        Location::Mtp(path) => mtp::create_dir(path),
    }
}

/// Copies a file to a new `target`, or in place of the existing one when `replace` is set.
fn copy_file(source: &Location, target: &Location, replace: bool) -> anyhow::Result<()> {
    match (source, target) {
        (_, Location::Local(target)) => copy_to_local(source, target, replace),
        (Location::Local(source), Location::Mtp(target)) => {
            send(&File::open(source)?, target, replace)
        }
        (Location::Mtp(source), Location::Mtp(target)) => {
            let buffer = tempfile::tempfile()?;
            mtp::receive_file(source, &buffer)?;
            send(&buffer, target, replace)
        }
    }
}

fn send(source: &File, target: &MtpPath, replace: bool) -> anyhow::Result<()> {
    if replace {
        mtp::replace_file(source, target)
    } else {
        mtp::send_file(source, target)
    }
}

/// Writes next to `target` under a hidden name and renames it at the end, so an
/// interrupted copy never looks like a complete file, and a replaced file stays intact
/// until its new version is complete. The rename is atomic, which MTP lacks. Data and
/// rename are synced to disk before this returns, so a crash cannot undo a reported copy.
fn copy_to_local(source: &Location, target: &Path, replace: bool) -> anyhow::Result<()> {
    ensure!(replace || !fs::exists(target)?, "already exists");
    let part = part_path(target)?;
    let result = write_local(source, &part).and_then(|()| Ok(fs::rename(&part, target)?));
    if result.is_err() {
        _ = fs::remove_file(&part);
    }
    result?;
    sync_parent(target)
}

fn write_local(source: &Location, part: &Path) -> anyhow::Result<()> {
    let file = match source {
        Location::Local(source) => {
            fs::copy(source, part)?;
            File::open(part)?
        }
        Location::Mtp(source) => {
            let file = File::create(part)?;
            mtp::receive_file(source, &file)?;
            file
        }
    };
    // On disk before the rename makes it look complete.
    file.sync_all()?;
    Ok(())
}

/// Makes the entry `path` just got in its directory, by creation or rename, survive a
/// crash.
fn sync_parent(path: &Path) -> anyhow::Result<()> {
    let parent = path.parent().context("no parent directory")?;
    File::open(parent)?.sync_all()?;
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

    const TO_RIGHT: Action = Action::Copy {
        target: Side::Right,
    };

    #[test]
    fn overwrite_replaces_only_changed_files_on_the_right() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let (left, right) = (temp.path().join("left"), temp.path().join("right"));
        fs::create_dir_all(left.join("Album"))?;
        fs::create_dir_all(right.join("Album"))?;
        fs::write(left.join("Album/song.mp3"), "new tags")?;
        fs::write(right.join("Album/song.mp3"), "old")?;
        fs::write(left.join("Album/same.mp3"), "same")?;
        fs::write(right.join("Album/same.mp3"), "same")?;
        fs::write(left.join("Album/only-left.mp3"), "missing")?;
        let mut diff = compare(&left, &right);

        let plan = Plan::new(&diff, Action::Overwrite, None);
        let names = plan
            .ops
            .iter()
            .map(|op| diff.nodes[op.node].name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["song.mp3"]);
        assert_eq!(plan.bytes, "new tags".len() as u64);

        let report = execute(&plan, &JobProgress::default());

        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(
            fs::read_to_string(right.join("Album/song.mp3"))?,
            "new tags"
        );
        assert!(!fs::exists(right.join("Album/only-left.mp3"))?);
        let leftovers = fs::read_dir(right.join("Album"))?.count();
        assert_eq!(leftovers, 2, "no part file stays behind");
        diff.mark_copied(&report.copied);
        assert_eq!(diff.counts().changed, 0);
        assert!(!has_work(&diff, Action::Overwrite, None));
        Ok(())
    }

    #[test]
    fn failed_overwrite_without_the_old_version_reports_it_removed() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let (left, right) = (temp.path().join("left"), temp.path().join("right"));
        build(&left, &["song.mp3", "kept.mp3"])?;
        fs::create_dir_all(&right)?;
        fs::write(right.join("song.mp3"), "old")?;
        fs::write(right.join("kept.mp3"), "old")?;
        let diff = compare(&left, &right);
        let plan = Plan::new(&diff, Action::Overwrite, None);
        // Both fail for lack of a source; only one also lost its old version meanwhile.
        fs::remove_file(left.join("song.mp3"))?;
        fs::remove_file(left.join("kept.mp3"))?;
        fs::remove_file(right.join("song.mp3"))?;

        let report = execute(&plan, &JobProgress::default());

        assert_eq!(report.failed.len(), 2);
        let removed = report
            .removed
            .iter()
            .map(|&node| diff.nodes[node].name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(removed, ["song.mp3"]);
        Ok(())
    }

    #[test]
    fn overwrite_needs_only_the_peak_growth_of_the_target() {
        // Grows by 5, then stays the same size.
        let files = [("grows.flac", 9, 4), ("retagged.flac", 10, 10)];
        let plan = |right: Location| {
            let mut diff = TreeDiff::new(Location::local("/left".into()), right);
            let found = files.into_iter().enumerate();
            diff.extend(found.map(|(index, (name, left, right))| {
                crate::diff::Found::Node(Node {
                    name: name.to_owned(),
                    file_name: name.into(),
                    kind: EntryKind::File { size: left },
                    presence: Presence::Both,
                    depth: 0,
                    parent: None,
                    content: Some(Content::Changed),
                    right_size: Some(right),
                    end: index + 1,
                    differs: true,
                })
            }));
            Plan::new(&diff, Action::Overwrite, None)
        };
        let phone = Location::Mtp(MtpPath {
            device: "phone".to_owned(),
            storage: "storage".to_owned(),
            dirs: Vec::new(),
        });

        let local = plan(Location::local("/right".into()));
        let mtp = plan(phone);

        assert_eq!(local.bytes, 19);
        // The 10-byte file is written next to its old version after the target grew by 5.
        assert_eq!(local.space, 15);
        // The old version goes first, so only the growth counts.
        assert_eq!(mtp.space, 5);
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

        let plan = Plan::new(&diff, TO_RIGHT, None);
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

        let report = execute(&plan, &JobProgress::default());

        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(
            fs::read_to_string(right.join("New/Deep/x.mp3"))?,
            "New/Deep/x.mp3"
        );
        assert!(has_work(&diff, TO_RIGHT, None));
        diff.mark_copied(&report.copied);
        assert!(!has_work(&diff, TO_RIGHT, None));
        let to_left = Action::Copy { target: Side::Left };
        assert!(
            has_work(&diff, to_left, None),
            "only-right.mp3 is still missing"
        );
        let rescanned = compare(&left, &right);
        assert_eq!(diff.counts().missing, rescanned.counts().missing);
        assert_eq!(rescanned.counts().missing.right, 0);
        assert_eq!(rescanned.counts().missing.left, 1);
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

        let report = execute(&Plan::new(&diff, TO_RIGHT, None), &JobProgress::default());

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

        let to_right = a.map(|a| Plan::new(&diff, TO_RIGHT, Some(a)));
        let to_left = a.map(|a| {
            let action = Action::Copy { target: Side::Left };
            Plan::new(&diff, action, Some(a))
        });

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
