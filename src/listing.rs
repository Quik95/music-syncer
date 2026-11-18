use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
}

/// Immediate children of a directory, or the error that prevented reading it.
#[derive(Debug)]
pub struct Listing {
    pub path: PathBuf,
    pub entries: io::Result<Vec<Entry>>,
}

impl Listing {
    /// Relative paths are made absolute against the current directory; symlinks stay unresolved.
    pub fn read(path: PathBuf) -> Self {
        let path = std::path::absolute(&path).unwrap_or(path);
        let entries = read_entries(&path);
        if let Err(error) = &entries {
            tracing::warn!(path = %path.display(), %error, "cannot list directory");
        }
        Self { path, entries }
    }
}

/// Directories first, then files; each group by case-insensitive name.
/// Symlinks are classified by their target.
fn read_entries(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = fs::read_dir(dir)?
        .map(|entry| {
            let entry = entry?;
            let kind = if entry.path().is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            Ok(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_cached_key(|entry| (entry.kind, entry.name.to_lowercase(), entry.name.clone()));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_come_first_then_names_case_insensitively() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        fs::create_dir(root.join("b-dir"))?;
        fs::create_dir(root.join("Z-dir"))?;
        fs::write(root.join("a.flac"), "")?;
        fs::write(root.join("C.mp3"), "")?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("b-dir"), root.join("link-to-dir"))?;

        let listing = Listing::read(root.to_path_buf());

        let names = listing
            .entries?
            .into_iter()
            .map(|entry| (entry.kind, entry.name))
            .collect::<Vec<_>>();
        let mut expected = vec![
            (EntryKind::Directory, "b-dir".to_owned()),
            (EntryKind::Directory, "Z-dir".to_owned()),
            (EntryKind::File, "a.flac".to_owned()),
            (EntryKind::File, "C.mp3".to_owned()),
        ];
        #[cfg(unix)]
        expected.insert(1, (EntryKind::Directory, "link-to-dir".to_owned()));
        assert_eq!(names, expected);
        Ok(())
    }
}
