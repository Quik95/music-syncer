//! Access to phones through KDE's `kmtpd`, which owns connected MTP devices while Plasma
//! runs, so this works alongside Dolphin.

use std::{ffi::OsString, fs::File, io, os::fd::AsFd as _, sync::OnceLock};

use anyhow::{Context as _, bail, ensure};
use zbus::{
    blocking::Connection,
    zvariant::{Fd, OwnedObjectPath},
};

use crate::{
    listing::{Entry, EntryKind},
    location::MtpPath,
};

const SERVICE: &str = "org.kde.kmtpd5";

#[zbus::proxy(
    interface = "org.kde.kmtp.Daemon",
    default_service = "org.kde.kmtpd5",
    default_path = "/modules/kmtpd",
    gen_async = false
)]
trait Daemon {
    #[zbus(name = "listDevices")]
    fn list_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.kde.kmtp.Device",
    default_service = "org.kde.kmtpd5",
    gen_async = false
)]
trait Device {
    #[zbus(name = "listStorages")]
    fn list_storages(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(property, name = "friendlyName")]
    fn friendly_name(&self) -> zbus::Result<String>;
}

/// `(item id, parent id, storage id, name, size, modification time, MIME type)`.
type MtpFile = (u32, u32, u32, String, u64, i64, String);

#[zbus::proxy(
    interface = "org.kde.kmtp.Storage",
    default_service = "org.kde.kmtpd5",
    gen_async = false
)]
trait Storage {
    /// Entries of the directory at `path`, and kmtpd's status (0 on success).
    #[zbus(name = "getFilesAndFolders")]
    fn get_files_and_folders(&self, path: &str) -> zbus::Result<(Vec<MtpFile>, i32)>;

    /// Item id 0 means no such file; use [`file_metadata`].
    #[zbus(name = "getFileMetadata")]
    fn get_file_metadata(&self, path: &str) -> zbus::Result<MtpFile>;

    /// New folder's id; 0 on failure, `u32::MAX` when it already exists and
    /// `u32::MAX - 1` when a file has its name.
    #[zbus(name = "createFolder")]
    fn create_folder(&self, path: &str) -> zbus::Result<u32>;

    /// Starts writing the file at `source_path` into `descriptor`; 0 when started.
    /// The outcome arrives as `copyFinished`.
    #[zbus(name = "getFileToFileDescriptor")]
    fn get_file_to_file_descriptor(
        &self,
        descriptor: Fd<'_>,
        source_path: &str,
    ) -> zbus::Result<i32>;

    /// Starts reading a new file at `destination_path` from `descriptor`; 0 when started.
    /// The outcome arrives as `copyFinished`.
    #[zbus(name = "sendFileFromFileDescriptor")]
    fn send_file_from_file_descriptor(
        &self,
        descriptor: Fd<'_>,
        destination_path: &str,
    ) -> zbus::Result<i32>;

    /// libmtp's result; 0 on success.
    #[zbus(name = "deleteObject")]
    fn delete_object(&self, path: &str) -> zbus::Result<i32>;

    /// libmtp's result of the transfer started last; 0 on success.
    #[zbus(signal, name = "copyFinished")]
    fn copy_finished(&self, result: i32) -> zbus::Result<()>;

    #[zbus(property, name = "description")]
    fn description(&self) -> zbus::Result<String>;

    #[zbus(property, name = "freeSpaceInBytes")]
    fn free_space_in_bytes(&self) -> zbus::Result<u64>;
}

/// Unsorted entries of an MTP directory.
pub fn read_dir(path: &MtpPath) -> io::Result<Vec<Entry>> {
    let storage = storage(path).map_err(io::Error::other)?;
    let storage_path = path.storage_path();
    let (files, status) = storage
        .get_files_and_folders(&storage_path)
        .map_err(io::Error::other)?;
    if status != 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("kmtpd cannot list {storage_path} (status {status})"),
        ));
    }
    Ok(files
        .into_iter()
        .map(|(.., name, size, _, mime)| Entry {
            kind: if mime == "inode/directory" {
                EntryKind::Directory
            } else {
                EntryKind::File { size }
            },
            file_name: OsString::from(&name),
            name,
        })
        .collect())
}

/// Creates a directory whose parent exists; an existing directory counts as success.
pub fn create_dir(path: &MtpPath) -> anyhow::Result<()> {
    match storage(path)?.create_folder(&path.storage_path())? {
        0 => bail!("kmtpd cannot create the folder"),
        u32::MAX => Ok(()),
        id if id == u32::MAX - 1 => bail!("a file with this name exists"),
        _ => Ok(()),
    }
}

/// Uploads `source` as a new file at `target`, which must not exist yet.
pub fn send_file(source: &File, target: &MtpPath) -> anyhow::Result<()> {
    let storage = storage(target)?;
    let target_path = target.storage_path();
    ensure!(
        file_metadata(&storage, &target_path)?.is_none(),
        "already exists on the phone"
    );
    upload(&storage, source, &target_path)
}

/// Uploads `source` in place of the file at `target`. MTP can neither overwrite nor rename
/// atomically, so the old version is deleted first; when the upload then fails, `target`
/// is left missing rather than incomplete, and a retry just uploads.
pub fn replace_file(source: &File, target: &MtpPath) -> anyhow::Result<()> {
    let storage = storage(target)?;
    let target_path = target.storage_path();
    if file_metadata(&storage, &target_path)?.is_some() {
        delete(&storage, &target_path).context("cannot delete the old version")?;
    }
    upload(&storage, source, &target_path)
        .context("the old version is deleted, but the new one did not arrive")
}

/// Uploads `source` as a new file at `path`. When the transfer fails, whatever arrived
/// of it is deleted.
fn upload(storage: &StorageProxy<'_>, source: &File, path: &str) -> anyhow::Result<()> {
    let result = transfer(storage, source, path);
    if result.is_err()
        && file_metadata(storage, path).is_ok_and(|file| file.is_some())
        && let Err(error) = delete(storage, path)
    {
        tracing::warn!(
            path,
            error = format!("{error:#}"),
            "cannot delete an incomplete upload"
        );
    }
    result
}

fn transfer(storage: &StorageProxy<'_>, source: &File, path: &str) -> anyhow::Result<()> {
    // Subscribed before starting so the signal cannot be missed.
    let finished = storage.receive_copy_finished()?;
    let status = storage.send_file_from_file_descriptor(Fd::from(source.as_fd()), path)?;
    ensure!(status == 0, "kmtpd refused the upload (status {status})");
    wait(finished)
}

fn delete(storage: &StorageProxy<'_>, path: &str) -> anyhow::Result<()> {
    let status = storage.delete_object(path)?;
    ensure!(status == 0, "kmtpd cannot delete {path} (status {status})");
    Ok(())
}

/// Downloads the file at `source` into `target`.
pub fn receive_file(source: &MtpPath, target: &File) -> anyhow::Result<()> {
    let storage = storage(source)?;
    let finished = storage.receive_copy_finished()?;
    let status =
        storage.get_file_to_file_descriptor(Fd::from(target.as_fd()), &source.storage_path())?;
    ensure!(status == 0, "kmtpd refused the download (status {status})");
    wait(finished)
}

/// The file or folder at `path`, `None` when there is none.
fn file_metadata(storage: &StorageProxy<'_>, path: &str) -> zbus::Result<Option<MtpFile>> {
    let file = storage.get_file_metadata(path)?;
    Ok((file.0 != 0).then_some(file))
}

/// Whether a file or folder exists at `path`.
pub fn exists(path: &MtpPath) -> anyhow::Result<bool> {
    Ok(file_metadata(&storage(path)?, &path.storage_path())?.is_some())
}

pub fn free_space(path: &MtpPath) -> anyhow::Result<u64> {
    Ok(storage(path)?.free_space_in_bytes()?)
}

/// kmtpd does not tag the signal with the transfer, so one started meanwhile by another
/// client on the same storage could be taken for ours.
fn wait(mut finished: copyFinishedIterator) -> anyhow::Result<()> {
    let signal = finished
        .next()
        .context("kmtpd stopped before finishing the transfer")?;
    let result = signal.args()?.result;
    ensure!(result == 0, "transfer failed (libmtp result {result})");
    Ok(())
}

/// The storage of a connected device, both matched by the names in `mtp:/` URLs.
fn storage(path: &MtpPath) -> anyhow::Result<StorageProxy<'static>> {
    let connection = connection()?;
    for device in DaemonProxy::new(&connection)?.list_devices()? {
        let device = DeviceProxy::builder(&connection).path(device)?.build()?;
        if device.friendly_name()? != path.device {
            continue;
        }
        for storage in device.list_storages()? {
            let storage = StorageProxy::builder(&connection).path(storage)?.build()?;
            if storage.description()? == path.storage {
                return Ok(storage);
            }
        }
        bail!("{} has no storage named {}", path.device, path.storage);
    }
    bail!(
        "{} is not connected ({SERVICE} does not list it)",
        path.device
    )
}

fn connection() -> zbus::Result<Connection> {
    static CONNECTION: OnceLock<Connection> = OnceLock::new();
    if let Some(connection) = CONNECTION.get() {
        return Ok(connection.clone());
    }
    let connection = Connection::session()?;
    Ok(CONNECTION.get_or_init(|| connection).clone())
}
