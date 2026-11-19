use std::{sync::Arc, time::Duration};

use gpui::{AnyElement, ObjectFit, RenderImage, div, img, prelude::*, px};
use image::{Frame, RgbaImage};

use super::theme;
use crate::track::Track;

/// Cover edge in logical pixels; decoded at twice that for HiDPI screens.
const COVER_SIZE: f32 = 128.;
pub const COVER_PIXELS: u32 = 256;

/// What the preview panel shows for the active audio file.
pub enum Preview {
    Loaded {
        track: Box<Track>,
        cover: Option<Arc<RenderImage>>,
    },
    Failed(String),
}

impl Preview {
    /// Moves the decoded cover into a GPU-ready image.
    pub fn loaded(mut track: Box<Track>) -> Self {
        let cover = track.cover.take().map(render_image);
        Self::Loaded { track, cover }
    }

    /// The image to release from the sprite atlas once this preview is gone.
    pub fn take_cover(&mut self) -> Option<Arc<RenderImage>> {
        match self {
            Self::Loaded { cover, .. } => cover.take(),
            Self::Failed(_) => None,
        }
    }
}

fn render_image(mut cover: RgbaImage) -> Arc<RenderImage> {
    // GPUI expects BGRA.
    for pixel in cover.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![Frame::new(cover)]))
}

pub fn render(preview: &Preview) -> AnyElement {
    let panel = div()
        .flex()
        .gap_3()
        .p_2()
        .border_t_1()
        .border_color(*theme::BORDER)
        .bg(*theme::HEADER_BACKGROUND);
    match preview {
        Preview::Loaded { track, cover } => {
            let frame = div()
                .flex_none()
                .size(px(COVER_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .bg(*theme::BACKGROUND);
            let frame = match cover {
                Some(image) => frame.child(
                    img(image.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                ),
                None => frame.text_color(*theme::TEXT_MUTED).child("No cover"),
            };
            panel.child(frame).child(details(track)).into_any_element()
        }
        Preview::Failed(error) => panel
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(*theme::ERROR)
                    .child(error.clone()),
            )
            .into_any_element(),
    }
}

fn details(track: &Track) -> impl IntoElement {
    let line = |text: String| div().truncate().child(text);
    let muted = |text: String| line(text).text_color(*theme::TEXT_MUTED);
    let mut column = div().flex().flex_col().min_w_0().flex_1().child(
        line(
            track
                .title
                .clone()
                .unwrap_or_else(|| "Unknown title".into()),
        )
        .text_lg()
        .font_weight(gpui::FontWeight::BOLD),
    );
    column = column.child(line(
        track
            .artist
            .clone()
            .unwrap_or_else(|| "Unknown artist".into()),
    ));
    if let Some(album) = album_line(track) {
        column = column.child(line(album));
    }
    if let Some(artist) = track
        .album_artist
        .as_ref()
        .filter(|album_artist| track.artist.as_ref() != Some(*album_artist))
    {
        column = column.child(muted(format!("Album artist: {artist}")));
    }
    if let Some(position) = position_line(track) {
        column = column.child(muted(position));
    }
    column.child(muted(stream_line(track)))
}

/// "Album (Year)".
fn album_line(track: &Track) -> Option<String> {
    match (&track.album, track.year) {
        (Some(album), Some(year)) => Some(format!("{album} ({year})")),
        (Some(album), None) => Some(album.clone()),
        (None, Some(year)) => Some(year.to_string()),
        (None, None) => None,
    }
}

/// "Track 3/12 · Disc 1/2 · Genre".
fn position_line(track: &Track) -> Option<String> {
    let numbered = |label: &str, number: Option<u32>, total: Option<u32>| {
        number.map(|number| match total {
            Some(total) => format!("{label} {number}/{total}"),
            None => format!("{label} {number}"),
        })
    };
    let parts = [
        numbered("Track", track.track, track.track_total),
        numbered("Disc", track.disc, track.disc_total),
        track.genre.clone(),
    ];
    let parts = parts.into_iter().flatten().collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// "3:45 · FLAC · 44.1 kHz · 16-bit · stereo · 1012 kbps".
fn stream_line(track: &Track) -> String {
    let parts = [
        Some(duration(track.duration)),
        Some(track.format.to_owned()),
        track
            .sample_rate
            .map(|rate| format!("{} kHz", rate as f32 / 1000.)),
        track.bit_depth.map(|depth| format!("{depth}-bit")),
        track.channels.map(|channels| match channels {
            1 => "mono".to_owned(),
            2 => "stereo".to_owned(),
            n => format!("{n} ch"),
        }),
        track.bitrate_kbps.map(|kbps| format!("{kbps} kbps")),
    ];
    parts.into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// "m:ss", or "h:mm:ss" from an hour up.
fn duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}
