//! Read access to phones through KDE's `kmtpd`, which owns connected MTP devices while
//! Plasma runs, so this works alongside Dolphin.

use std::{ffi::OsString, io, sync::OnceLock};

use zbus::{blocking::Connection, zvariant::OwnedObjectPath};

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
type File = (u32, u32, u32, String, u64, i64, String);

#[zbus::proxy(
    interface = "org.kde.kmtp.Storage",
    default_service = "org.kde.kmtpd5",
    gen_async = false
)]
trait Storage {
    /// Entries of the directory at `path`, and kmtpd's status (0 on success).
    #[zbus(name = "getFilesAndFolders")]
    fn get_files_and_folders(&self, path: &str) -> zbus::Result<(Vec<File>, i32)>;

    #[zbus(property, name = "description")]
    fn description(&self) -> zbus::Result<String>;
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
        .map(|(.., name, _, _, mime)| Entry {
            kind: if mime == "inode/directory" {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            file_name: OsString::from(&name),
            name,
        })
        .collect())
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
        anyhow::bail!("{} has no storage named {}", path.device, path.storage);
    }
    anyhow::bail!(
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
