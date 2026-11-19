use std::{
    ffi::{OsStr, OsString},
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
    /// Exact name on disk, for building paths.
    pub file_name: OsString,
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

    /// Path of the entry at `index` if it is a directory.
    pub fn subdirectory(&self, index: usize) -> Option<PathBuf> {
        let entry = self.entries.as_ref().ok()?.get(index)?;
        (entry.kind == EntryKind::Directory).then(|| self.path.join(&entry.file_name))
    }

    /// Path of the entry at `index` if it is a file.
    pub fn file(&self, index: usize) -> Option<PathBuf> {
        let entry = self.entries.as_ref().ok()?.get(index)?;
        (entry.kind == EntryKind::File).then(|| self.path.join(&entry.file_name))
    }

    /// Index of the entry with exactly this on-disk name.
    pub fn position(&self, file_name: &OsStr) -> Option<usize> {
        self.entries
            .as_ref()
            .ok()?
            .iter()
            .position(|entry| entry.file_name == file_name)
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
            let file_name = entry.file_name();
            Ok(Entry {
                name: file_name.to_string_lossy().into_owned(),
                file_name,
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

    #[test]
    fn subdirectory_is_only_for_directories() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir(temp.path().join("dir"))?;
        fs::write(temp.path().join("file"), "")?;

        let listing = Listing::read(temp.path().to_path_buf());

        assert_eq!(listing.subdirectory(0), Some(temp.path().join("dir")));
        assert_eq!(listing.subdirectory(1), None);
        assert_eq!(listing.subdirectory(2), None);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_directory_resolves_to_its_real_path() -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir()?;
        let name = OsStr::from_bytes(b"caf\xe9");
        fs::create_dir(temp.path().join(name))?;

        let listing = Listing::read(temp.path().to_path_buf());

        let child = listing.subdirectory(0);
        assert_eq!(child, Some(temp.path().join(name)));
        assert!(child.is_some_and(|path| path.is_dir()));
        assert_eq!(listing.position(name), Some(0));
        Ok(())
    }
}
