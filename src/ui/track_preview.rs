use std::{sync::Arc, time::Duration};

use gpui_kit::{
    AnyElement, App, FontWeight, ObjectFit, RenderImage,
    assets::IconName,
    component::{
        ActiveTheme as _, Icon, Sizable as _, alert::Alert, h_flex, label::Label, tag::Tag, v_flex,
    },
    div, img,
    prelude::*,
    px,
};
use image::{Frame, RgbaImage};

use crate::track::{Position, Track};

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
    /// Turns the decoded cover into a GPU-ready image.
    pub fn loaded(track: Box<Track>, cover: Option<RgbaImage>) -> Self {
        Self::Loaded {
            track,
            cover: cover.map(render_image),
        }
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

pub fn render(preview: &Preview, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let panel = h_flex()
        .items_start()
        .gap_3()
        .p_2()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.title_bar);
    match preview {
        Preview::Loaded { track, cover } => {
            let frame = div()
                .flex_none()
                .size(px(COVER_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius)
                .bg(theme.background);
            let frame = match cover {
                Some(image) => frame.child(
                    img(image.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                ),
                None => frame.child(
                    // Registered in `super::assets`; outside the kit's default icons.
                    Icon::new(IconName::Disc3)
                        .large()
                        .text_color(theme.muted_foreground),
                ),
            };
            panel
                .child(frame)
                .child(details(track, cx))
                .into_any_element()
        }
        Preview::Failed(error) => panel
            .child(
                Alert::error("preview-error", error.clone())
                    .title("Cannot read metadata")
                    .flex_1(),
            )
            .into_any_element(),
    }
}

fn details(track: &Track, cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    // One `Label::new` call site for every line, so each needs its own id: the default one
    // is that call site.
    let line = |id: &'static str, text: String| Label::new(text).id(id).truncate();
    let mut column = v_flex().min_w_0().flex_1().gap_0p5().child(
        line(
            "title",
            track
                .title
                .clone()
                .unwrap_or_else(|| "Unknown title".into()),
        )
        .text_lg()
        .font_weight(FontWeight::BOLD),
    );
    column = column.child(line(
        "artist",
        track
            .artist
            .clone()
            .unwrap_or_else(|| "Unknown artist".into()),
    ));
    if let Some(album) = album_line(track) {
        column = column.child(line("album", album));
    }
    if let Some(artist) = track
        .album_artist
        .as_ref()
        .filter(|album_artist| track.artist.as_ref() != Some(*album_artist))
    {
        column =
            column.child(line("album-artist", format!("Album artist: {artist}")).text_color(muted));
    }
    if let Some(position) = position_line(track) {
        column = column.child(line("position", position).text_color(muted));
    }
    let tags = stream_parts(track)
        .into_iter()
        .map(|part| Tag::secondary().small().child(part));
    column.child(h_flex().flex_wrap().gap_1().pt_1().children(tags))
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
    let numbered = |label: &str, position: Option<Position>| {
        position.map(|Position { number, total }| match total {
            Some(total) => format!("{label} {number}/{total}"),
            None => format!("{label} {number}"),
        })
    };
    let parts = [
        numbered("Track", track.track),
        numbered("Disc", track.disc),
        track.genre.clone(),
    ];
    let parts = parts.into_iter().flatten().collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// "3:45", "FLAC", "44.1 kHz", "16-bit", "stereo", "1012 kbps".
fn stream_parts(track: &Track) -> impl IntoIterator<Item = String> {
    [
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
    ]
    .into_iter()
    .flatten()
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
