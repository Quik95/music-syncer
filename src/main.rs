use std::{ffi::OsString, process::ExitCode};

use directories::UserDirs;

mod cursor;
mod listing;
mod location;
mod logging;
mod mtp;
mod sync;
mod track;
mod tree_diff;
mod ui;

use listing::Listing;
use location::Location;

const USAGE: &str = "usage: music-syncer [LEFT RIGHT]  (local paths or mtp:/DEVICE/STORAGE/… URLs)";

fn main() -> ExitCode {
    logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");

    let (left, right) = match column_locations(std::env::args_os().skip(1)) {
        Ok(Some(locations)) => locations,
        Ok(None) => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("{error:#}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(%left, %right, "listing directories");

    ui::run(Listing::read(left), Listing::read(right));
    ExitCode::SUCCESS
}

/// Both locations from the command line, or the XDG music directory and home when none
/// are given. `None` for a wrong number of arguments.
fn column_locations(
    mut args: impl Iterator<Item = OsString>,
) -> anyhow::Result<Option<(Location, Location)>> {
    match (args.next(), args.next(), args.next()) {
        (Some(left), Some(right), None) => Ok(Some((
            Location::from_arg(left)?,
            Location::from_arg(right)?,
        ))),
        (None, _, _) => {
            let Some(dirs) = UserDirs::new() else {
                return Ok(None);
            };
            let home = dirs.home_dir().to_path_buf();
            let music = dirs.audio_dir().map_or_else(|| home.clone(), Into::into);
            Ok(Some((Location::local(music), Location::local(home))))
        }
        _ => Ok(None),
    }
}
