//! Comparing the bytes of files that exist on both sides of a [`TreeDiff`] with equal sizes.

use std::{
    fs::File,
    io::{self, BufRead as _, BufReader, Read, Seek as _},
};

use anyhow::Context as _;

use super::{Content, Side, TreeDiff};
use crate::{
    progress::JobProgress,
    storage::{listing::EntryKind, location::Location, mtp},
};

const BUFFER: usize = 256 * 1024;

/// How a pair compared, or why it could not be compared.
pub type Verdict = anyhow::Result<Content>;

/// One pair of files to compare.
#[derive(Debug)]
pub struct Check {
    pub node: usize,
    left: Location,
    right: Location,
    size: u64,
}

#[derive(Debug)]
pub struct CheckPlan {
    pub checks: Vec<Check>,
    /// Size of one file of each pair, summed.
    pub bytes: u64,
}

impl CheckPlan {
    /// Files within `scope` (the whole tree when `None`) whose contents were not compared
    /// yet.
    pub fn new(diff: &TreeDiff, scope: Option<usize>) -> Self {
        let checks: Vec<_> = candidates(diff, scope)
            .map(|node| Check {
                node,
                left: diff.location(node, Side::Left),
                right: diff.location(node, Side::Right),
                size: match diff.nodes[node].kind {
                    EntryKind::File { size } => size,
                    EntryKind::Directory => 0,
                },
            })
            .collect();
        let bytes = checks.iter().map(|check| check.size).sum();
        Self { checks, bytes }
    }
}

/// Whether [`CheckPlan::new`] would find anything, without building the plan: from the
/// counts for the whole tree, else by scanning the subtree.
pub fn has_work(diff: &TreeDiff, scope: Option<usize>) -> bool {
    match scope {
        None => diff.counts().unchecked > 0,
        Some(_) => candidates(diff, scope).next().is_some(),
    }
}

fn candidates(diff: &TreeDiff, scope: Option<usize>) -> impl Iterator<Item = usize> {
    let range = scope.map_or(0..diff.nodes.len(), |node| node..diff.nodes[node].end);
    range.filter(|&node| diff.nodes[node].content == Some(Content::Unchecked))
}

/// Compares each pair in order, passing every verdict to `checked` as soon as it is known.
/// A pair that cannot be read is passed as an error and the rest goes on.
pub fn execute(plan: &CheckPlan, progress: &JobProgress, checked: &mut dyn FnMut(usize, Verdict)) {
    for check in &plan.checks {
        if progress.is_cancelled() {
            break;
        }
        progress.start(
            check
                .left
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        );
        let verdict = same_contents(&check.left, &check.right).map(|same| {
            if same {
                Content::Same
            } else {
                Content::Changed
            }
        });
        if let Err(error) = &verdict {
            tracing::warn!(left = %check.left, right = %check.right, error = format!("{error:#}"), "cannot compare contents");
        }
        checked(check.node, verdict);
        progress.finish(check.size);
    }
}

fn same_contents(left: &Location, right: &Location) -> anyhow::Result<bool> {
    same_bytes(open(left)?, open(right)?).with_context(|| format!("comparing {left} with {right}"))
}

/// A local file as it is; an MTP file downloaded whole, as MTP cannot stream it. Errors
/// name `location`.
fn open(location: &Location) -> anyhow::Result<File> {
    let file = match location {
        Location::Local(path) => File::open(path).map_err(anyhow::Error::from),
        Location::Mtp(path) => {
            tempfile::tempfile()
                .map_err(anyhow::Error::from)
                .and_then(|mut file| {
                    mtp::receive_file(path, &file)?;
                    file.rewind()?;
                    Ok(file)
                })
        }
    };
    file.with_context(|| location.to_string())
}

/// Reads both in step and stops at the first difference.
fn same_bytes(left: impl Read, right: impl Read) -> io::Result<bool> {
    let mut left = BufReader::with_capacity(BUFFER, left);
    let mut right = BufReader::with_capacity(BUFFER, right);
    loop {
        let (left_bytes, right_bytes) = (left.fill_buf()?, right.fill_buf()?);
        if left_bytes.is_empty() || right_bytes.is_empty() {
            return Ok(left_bytes.is_empty() && right_bytes.is_empty());
        }
        let len = left_bytes.len().min(right_bytes.len());
        if left_bytes[..len] != right_bytes[..len] {
            return Ok(false);
        }
        left.consume(len);
        right.consume(len);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn equal_sized_files_are_checked_byte_by_byte() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let (left, right) = (temp.path().join("left"), temp.path().join("right"));
        for (side, edited) in [(&left, "tag: A"), (&right, "tag: B")] {
            fs::create_dir_all(side.join("Album"))?;
            fs::write(side.join("Album/edited.mp3"), edited)?;
            fs::write(side.join("Album/same.mp3"), "same bytes")?;
        }
        let diff = TreeDiff::compare(&Location::local(left), &Location::local(right));
        let plan = CheckPlan::new(&diff, None);
        assert_eq!(plan.checks.len(), 2);

        let mut verdicts = Vec::new();
        execute(&plan, &JobProgress::default(), &mut |node, verdict| {
            verdicts.push((
                diff.nodes[node].name.clone(),
                verdict.map_err(|error| error.to_string()),
            ));
        });

        assert_eq!(
            verdicts,
            [
                ("edited.mp3".to_owned(), Ok(Content::Changed)),
                ("same.mp3".to_owned(), Ok(Content::Same)),
            ]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_named_in_the_error() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir()?;
        let (left, right) = (temp.path().join("left"), temp.path().join("right"));
        for side in [&left, &right] {
            fs::create_dir(side)?;
            fs::write(side.join("song.mp3"), "bytes")?;
        }
        let locked = right.join("song.mp3");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))?;
        if File::open(&locked).is_ok() {
            // Running as root, which ignores permissions.
            return Ok(());
        }
        let diff = TreeDiff::compare(&Location::local(left), &Location::local(right));

        let mut errors = Vec::new();
        execute(
            &CheckPlan::new(&diff, None),
            &JobProgress::default(),
            &mut |_, verdict| errors.extend(verdict.err().map(|error| format!("{error:#}"))),
        );

        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].starts_with(&locked.display().to_string()),
            "{}",
            errors[0]
        );
        Ok(())
    }

    #[test]
    fn a_longer_stream_is_not_the_same() -> io::Result<()> {
        assert!(same_bytes(&b"abc"[..], &b"abc"[..])?);
        assert!(!same_bytes(&b"abc"[..], &b"abcd"[..])?);
        assert!(!same_bytes(&b"abcd"[..], &b"abc"[..])?);
        Ok(())
    }
}
