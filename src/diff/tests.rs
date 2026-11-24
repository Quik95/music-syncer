use std::{fs, io};

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
