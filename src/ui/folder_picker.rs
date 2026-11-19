//! Directory selection in the desktop's native dialog.
//!
//! On Plasma the KDE portal backend is called directly, because the xdg-desktop-portal
//! frontend drops every URI that is not `file://`, and KIO cannot turn phones' `mtp:/`
//! folders into local paths. Elsewhere the regular portal is used.

use std::{
    collections::HashMap,
    os::unix::ffi::OsStrExt as _,
    sync::atomic::{AtomicU32, Ordering},
};

use anyhow::Context as _;
use ashpd::{
    Error, WindowIdentifier,
    desktop::{ResponseError, file_chooser::OpenFileRequest},
};
use gpui::Window;
use raw_window_handle::{
    HasDisplayHandle as _, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};
use zbus::{
    names::ErrorName,
    zvariant::{ObjectPath, OwnedValue, Value},
};

use crate::location::Location;

const KDE_BACKEND: &str = "org.freedesktop.impl.portal.desktop.kde";
const TITLE: &str = "Open Folder";

/// Native handles of the window the dialog is attached to.
#[derive(Clone, Copy)]
pub struct Parent(Option<(RawWindowHandle, RawDisplayHandle)>);

impl Parent {
    pub fn of(window: &Window) -> Self {
        // `Window` has an inherent `window_handle` returning gpui's own handle type.
        let window_handle = HasWindowHandle::window_handle(window)
            .ok()
            .map(|handle| handle.as_raw());
        let display_handle = window.display_handle().ok().map(|handle| handle.as_raw());
        Self(window_handle.zip(display_handle))
    }
}

/// Opens a modal directory chooser starting in `start` when it is local. `None` means the
/// user cancelled.
///
/// Must run on the main thread while the parent window is open: on Wayland the raw
/// handles point at the window's live surface and display.
pub async fn pick_directory(parent: Parent, start: &Location) -> anyhow::Result<Option<Location>> {
    // Keeps the window exported to the dialog until it closes.
    let identifier = match &parent.0 {
        Some((window, display)) => WindowIdentifier::from_raw_handle(window, Some(display)).await,
        None => None,
    };
    let uri = match pick_with_kde(identifier.as_ref(), start).await {
        Err(PickError::NoKdeBackend) => pick_with_portal(identifier, start).await?,
        Err(PickError::Other(error)) => return Err(error),
        Ok(uri) => uri,
    };
    uri.map(|uri| Location::from_uri(&uri)).transpose()
}

enum PickError {
    NoKdeBackend,
    Other(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for PickError {
    fn from(error: E) -> Self {
        Self::Other(error.into())
    }
}

/// `org.freedesktop.impl.portal.FileChooser.OpenFile` on the KDE backend, which replies
/// once the dialog closes.
async fn pick_with_kde(
    identifier: Option<&WindowIdentifier>,
    start: &Location,
) -> Result<Option<String>, PickError> {
    static REQUESTS: AtomicU32 = AtomicU32::new(0);

    let connection = zbus::Connection::session().await?;
    let sender = connection
        .unique_name()
        .context("D-Bus connection has no unique name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let token = REQUESTS.fetch_add(1, Ordering::Relaxed);
    let handle = format!("/org/freedesktop/portal/desktop/request/{sender}/music_syncer_{token}");
    let handle = ObjectPath::try_from(handle.as_str())?;
    let parent_window = identifier.map(ToString::to_string).unwrap_or_default();

    let mut options = HashMap::from([
        ("directory", Value::from(true)),
        ("modal", Value::from(true)),
    ]);
    if let Some(path) = start.as_local() {
        let mut bytes = path.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        options.insert("current_folder", Value::from(bytes));
    }

    let reply = connection
        .call_method(
            Some(KDE_BACKEND),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.impl.portal.FileChooser"),
            "OpenFile",
            &(handle, "", parent_window.as_str(), TITLE, options),
        )
        .await;
    let reply = match reply {
        Err(zbus::Error::MethodError(name, ..)) if is_missing_service(&name) => {
            tracing::debug!("no KDE portal backend, using xdg-desktop-portal");
            return Err(PickError::NoKdeBackend);
        }
        reply => reply?,
    };
    let (response, mut results): (u32, HashMap<String, OwnedValue>) = reply.body().deserialize()?;
    match response {
        0 => {}
        1 => return Ok(None),
        _ => return Err(anyhow::anyhow!("file chooser failed (response {response})").into()),
    }
    let uris: Vec<String> = results
        .remove("uris")
        .context("file chooser returned no folder")?
        .try_into()?;
    let uri = uris
        .into_iter()
        .next()
        .context("file chooser returned no folder")?;
    Ok(Some(uri))
}

fn is_missing_service(name: &ErrorName<'_>) -> bool {
    matches!(
        name.as_str(),
        "org.freedesktop.DBus.Error.ServiceUnknown" | "org.freedesktop.DBus.Error.NameHasNoOwner"
    )
}

/// The xdg-desktop-portal file chooser, which only returns local folders.
async fn pick_with_portal(
    identifier: Option<WindowIdentifier>,
    start: &Location,
) -> anyhow::Result<Option<String>> {
    let mut request = OpenFileRequest::default()
        .identifier(identifier)
        .modal(true)
        .title(TITLE)
        .directory(true);
    if let Some(path) = start.as_local() {
        request = request.current_folder(path)?;
    }
    let request = request.send().await.map_err(|error| match error {
        Error::PortalNotFound(_) => {
            anyhow::anyhow!("no xdg-desktop-portal implementation provides a file chooser")
        }
        error => error.into(),
    })?;

    let files = match request.response() {
        Ok(files) => files,
        Err(Error::Response(ResponseError::Cancelled)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let uri = files
        .uris()
        .first()
        .context("file chooser returned no folder")?;
    Ok(Some(uri.to_string()))
}
