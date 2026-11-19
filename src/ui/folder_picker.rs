//! Directory selection through the XDG desktop portal's file chooser, which shows the
//! desktop's native dialog (KDE's on Plasma).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use ashpd::{
    Error, WindowIdentifier,
    desktop::{ResponseError, file_chooser::OpenFileRequest},
};
use gpui::Window;
use raw_window_handle::{
    HasDisplayHandle as _, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};

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

/// Opens a modal directory chooser starting in `start`. `None` means the user cancelled.
///
/// Must run on the main thread while the parent window is open: on Wayland the raw
/// handles point at the window's live surface and display.
pub async fn pick_directory(parent: Parent, start: &Path) -> anyhow::Result<Option<PathBuf>> {
    let identifier = match &parent.0 {
        Some((window, display)) => WindowIdentifier::from_raw_handle(window, Some(display)).await,
        None => None,
    };
    let request = OpenFileRequest::default()
        .identifier(identifier)
        .modal(true)
        .title("Open Folder")
        .directory(true)
        .current_folder(start)?
        .send()
        .await
        .map_err(|error| match error {
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
    let path = url::Url::parse(uri.as_str())
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .with_context(|| format!("file chooser returned a non-local folder: {}", uri.as_str()))?;

    Ok(Some(path))
}
