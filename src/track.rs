use std::{borrow::Cow, path::Path, time::Duration};

use image::RgbaImage;
use lofty::{
    file::{AudioFile as _, FileType, TaggedFileExt as _},
    picture::{Picture, PictureType},
    tag::{Accessor, ItemKey},
};

/// Tags and stream properties of one audio file.
#[derive(Debug)]
pub struct Track {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u16>,
    pub track: Option<Position>,
    pub disc: Option<Position>,
    pub format: &'static str,
    pub duration: Duration,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
}

/// Track or disc number, with the count in the set when tagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub number: u32,
    pub total: Option<u32>,
}

impl Position {
    /// A total without a number is dropped: it does not place the file anywhere.
    fn new(number: Option<u32>, total: Option<u32>) -> Option<Self> {
        number.map(|number| Self { number, total })
    }
}

/// Whether the file extension belongs to an audio format whose metadata can be read.
pub fn is_audio(path: &Path) -> bool {
    FileType::from_path(path).is_some()
}

/// Reads metadata of an audio file, and its embedded front cover (or first picture)
/// scaled to fit `cover_size`. An undecodable cover is logged and left out.
pub fn read(path: &Path, cover_size: u32) -> anyhow::Result<(Track, Option<RgbaImage>)> {
    let file = lofty::read_from_path(path)?;
    let tag = file.primary_tag().or_else(|| file.first_tag());
    let text = |value: Option<Cow<'_, str>>| value.map(Cow::into_owned);
    let properties = file.properties();
    let cover = file
        .tags()
        .iter()
        .find_map(|tag| cover_picture(tag.pictures()))
        .and_then(|picture| match image::load_from_memory(picture.data()) {
            Ok(image) => Some(image.thumbnail(cover_size, cover_size).into_rgba8()),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "cannot decode cover art");
                None
            }
        });

    let track = Track {
        title: text(tag.and_then(Accessor::title)),
        artist: text(tag.and_then(Accessor::artist)),
        album: text(tag.and_then(Accessor::album)),
        album_artist: tag
            .and_then(|tag| tag.get_string(ItemKey::AlbumArtist))
            .map(str::to_owned),
        genre: text(tag.and_then(Accessor::genre)),
        year: tag.and_then(Accessor::date).map(|date| date.year),
        track: Position::new(
            tag.and_then(Accessor::track),
            tag.and_then(Accessor::track_total),
        ),
        disc: Position::new(
            tag.and_then(Accessor::disk),
            tag.and_then(Accessor::disk_total),
        ),
        format: format_name(file.file_type()),
        duration: properties.duration(),
        bitrate_kbps: properties.audio_bitrate().or(properties.overall_bitrate()),
        sample_rate: properties.sample_rate(),
        bit_depth: properties.bit_depth(),
        channels: properties.channels(),
    };
    Ok((track, cover))
}

fn cover_picture(pictures: &[Picture]) -> Option<&Picture> {
    pictures
        .iter()
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first())
}

fn format_name(file_type: FileType) -> &'static str {
    match file_type {
        FileType::Aac => "AAC",
        FileType::Aiff => "AIFF",
        FileType::Ape => "APE",
        FileType::Flac => "FLAC",
        FileType::Mpeg => "MP3",
        FileType::Mp4 => "MP4",
        FileType::Mpc => "Musepack",
        FileType::Opus => "Opus",
        FileType::Vorbis => "Vorbis",
        FileType::Speex => "Speex",
        FileType::Wav => "WAV",
        FileType::WavPack => "WavPack",
        _ => "Audio",
    }
}
