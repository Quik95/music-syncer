use std::{path::PathBuf, process::ExitCode};

use directories::UserDirs;

mod cursor;
mod listing;
mod logging;
mod track;
mod tree_diff;
mod ui;

use listing::Listing;

const USAGE: &str = "usage: music-syncer [LEFT_DIR RIGHT_DIR]";

fn main() -> ExitCode {
    logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");

    let Some((left, right)) = column_paths(std::env::args_os().skip(1).map(PathBuf::from)) else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    tracing::info!(left = %left.display(), right = %right.display(), "listing directories");

    ui::run(Listing::read(left), Listing::read(right));
    ExitCode::SUCCESS
}

/// Both paths from the command line, or the XDG music directory and home when none are given.
fn column_paths(mut args: impl Iterator<Item = PathBuf>) -> Option<(PathBuf, PathBuf)> {
    match (args.next(), args.next(), args.next()) {
        (Some(left), Some(right), None) => Some((left, right)),
        (None, _, _) => {
            let dirs = UserDirs::new()?;
            let home = dirs.home_dir().to_path_buf();
            let music = dirs.audio_dir().map_or_else(|| home.clone(), PathBuf::from);
            Some((music, home))
        }
        _ => None,
    }
}
