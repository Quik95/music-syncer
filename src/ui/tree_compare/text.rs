//! Wording of the status bar, dialogs and notices.

use gpui_kit::SharedString;

use crate::diff::{Missing, Side, TreeDiff};

pub(super) fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}

/// "3 files and 1 folder (12.3 MB)".
pub(super) fn describe(files: usize, dirs: usize, bytes: u64) -> String {
    let count =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts = Vec::new();
    if files > 0 {
        parts.push(count(files, "file", "files"));
    }
    if dirs > 0 {
        parts.push(count(dirs, "folder", "folders"));
    }
    format!("{} ({})", parts.join(" and "), format_bytes(bytes))
}

/// Decimal units, as in Dolphin: "1.2 GB".
pub(super) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    // Lossy for sizes above 2^53 bytes, far beyond any disk.
    let mut value = bytes as f64 / 1000.;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Differences found in `diff`. Equality is claimed only once the walk is `complete` and
/// every directory could be read; before that the counts are marked provisional. Files
/// whose bytes were not compared are listed apart, as their equal sizes prove nothing.
pub(super) fn summary(diff: &TreeDiff, complete: bool) -> SharedString {
    let counts = diff.counts();
    let Missing { left, right } = counts.missing;
    let changed = counts.changed;
    let same = left == 0 && right == 0 && changed == 0;
    let mut summary = match (complete, same) {
        (true, true) if diff.errors.is_empty() => "Same entries on both sides".to_owned(),
        (true, true) => "No differences found among readable entries".to_owned(),
        (true, false) => format!(
            "Missing on the left: {left} · Missing on the right: {right} · Changed: {changed}"
        ),
        (false, true) => "Comparing… no differences so far".to_owned(),
        (false, false) => format!(
            "Comparing… so far missing on the left: {left} · on the right: {right} · \
             changed: {changed}"
        ),
    };
    if complete && counts.unchecked > 0 {
        summary += &format!(" · Contents not compared: {}", counts.unchecked);
    }
    if let Some(first) = diff.errors.first() {
        summary += &format!(
            " · {} unreadable, first: {}: {}",
            diff.errors.len(),
            first.path,
            first.message
        );
    }
    summary.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff::{Found, ReadError},
        storage::location::Location,
    };

    #[test]
    fn unreadable_directory_prevents_claiming_equality() {
        let local = |path: &str| Location::local(path.into());
        let mut diff = TreeDiff::new(local("/left"), local("/right"));
        diff.extend([Found::Error(ReadError {
            path: local("/left/Album"),
            message: "Permission denied".to_owned(),
        })]);

        let status = summary(&diff, true);

        assert!(!status.contains("Same entries"), "{status}");
        assert!(status.contains("1 unreadable"), "{status}");
    }

    #[test]
    fn bytes_use_decimal_units_without_rounding_up_to_1000() {
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_000), "1.0 kB");
        assert_eq!(format_bytes(1_234_567), "1.2 MB");
        assert_eq!(format_bytes(999_960), "1.0 MB");
        assert_eq!(format_bytes(52_400_000_000), "52.4 GB");
    }
}
