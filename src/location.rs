//! Where a listed directory lives: the local filesystem or a phone's MTP storage.

use std::{
    ffi::{OsStr, OsString},
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail};
use percent_encoding::percent_decode_str;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Local(PathBuf),
    Mtp(MtpPath),
}

/// A directory on an MTP storage, addressed like KIO's `mtp:/` URLs: device name,
/// storage description, then directories below the storage root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtpPath {
    pub device: String,
    pub storage: String,
    pub dirs: Vec<String>,
}

impl MtpPath {
    /// Path within the storage as kmtpd expects it, `/` for the root.
    pub fn storage_path(&self) -> String {
        format!("/{}", self.dirs.join("/"))
    }
}

impl Location {
    /// Relative paths are made absolute against the current directory and trailing slashes
    /// are dropped; symlinks stay unresolved.
    pub fn local(path: PathBuf) -> Self {
        let path = std::path::absolute(&path)
            .unwrap_or(path)
            .components()
            .collect();
        Self::Local(path)
    }

    /// A command-line argument: an `mtp:/` URL or a local path.
    pub fn from_arg(arg: OsString) -> anyhow::Result<Self> {
        match arg.to_str() {
            Some(url) if url.starts_with("mtp:") => Self::from_uri(url),
            _ => Ok(Self::local(PathBuf::from(arg))),
        }
    }

    /// A `file://` or KIO `mtp:/` URI as returned by the file chooser.
    pub fn from_uri(uri: &str) -> anyhow::Result<Self> {
        let url = url::Url::parse(uri).with_context(|| format!("invalid URI {uri}"))?;
        match url.scheme() {
            "file" => url
                .to_file_path()
                .map(Self::local)
                .map_err(|()| anyhow::anyhow!("not a local path: {uri}")),
            "mtp" => {
                let mut segments = url
                    .path_segments()
                    .into_iter()
                    .flatten()
                    .filter(|segment| !segment.is_empty())
                    .map(|segment| percent_decode_str(segment).decode_utf8_lossy().into_owned());
                let (Some(device), Some(storage)) = (segments.next(), segments.next()) else {
                    bail!("choose a folder inside a phone's storage, not {uri}");
                };
                Ok(Self::Mtp(MtpPath {
                    device,
                    storage,
                    dirs: segments.collect(),
                }))
            }
            scheme => bail!("unsupported location type {scheme}: {uri}"),
        }
    }

    pub fn join(&self, name: &OsStr) -> Self {
        match self {
            Self::Local(path) => Self::Local(path.join(name)),
            Self::Mtp(mtp) => {
                let mut mtp = mtp.clone();
                mtp.dirs.push(name.to_string_lossy().into_owned());
                Self::Mtp(mtp)
            }
        }
    }

    /// `None` at the filesystem root and at the root of an MTP storage.
    pub fn parent(&self) -> Option<Self> {
        match self {
            Self::Local(path) => path
                .parent()
                .map(|parent| Self::Local(parent.to_path_buf())),
            Self::Mtp(mtp) => {
                let mut mtp = mtp.clone();
                mtp.dirs.pop()?;
                Some(Self::Mtp(mtp))
            }
        }
    }

    pub fn file_name(&self) -> Option<&OsStr> {
        match self {
            Self::Local(path) => path.file_name(),
            Self::Mtp(mtp) => mtp.dirs.last().map(OsStr::new),
        }
    }

    pub fn as_local(&self) -> Option<&Path> {
        match self {
            Self::Local(path) => Some(path),
            Self::Mtp(_) => None,
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(path) => path.display().fmt(f),
            Self::Mtp(mtp) => {
                write!(f, "mtp:/{}/{}", mtp.device, mtp.storage)?;
                mtp.dirs.iter().try_for_each(|dir| write!(f, "/{dir}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kio_mtp_uri_splits_into_device_storage_and_directories() -> anyhow::Result<()> {
        let location = Location::from_uri("mtp:/moto%20g24%20power/android/Music/Gorillaz/")?;

        let Location::Mtp(mtp) = &location else {
            bail!("expected MTP, got {location:?}");
        };
        assert_eq!(mtp.device, "moto g24 power");
        assert_eq!(mtp.storage, "android");
        assert_eq!(mtp.dirs, ["Music", "Gorillaz"]);
        assert_eq!(mtp.storage_path(), "/Music/Gorillaz");
        assert_eq!(
            location.to_string(),
            "mtp:/moto g24 power/android/Music/Gorillaz"
        );
        Ok(())
    }

    #[test]
    fn mtp_parent_stops_at_storage_root() -> anyhow::Result<()> {
        let music = Location::from_uri("mtp:/phone/Internal%20shared%20storage/Music")?;

        let root = music.parent();
        assert_eq!(
            root.as_ref().map(ToString::to_string).as_deref(),
            Some("mtp:/phone/Internal shared storage")
        );
        assert_eq!(root.and_then(|root| root.parent()), None);
        assert!(Location::from_uri("mtp:/phone").is_err());
        Ok(())
    }
}
