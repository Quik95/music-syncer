use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use directories::ProjectDirs;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

const APP_NAME: &str = "music-syncer";
const DEFAULT_FILTER: &str = "info";
const KEPT_RUNS: usize = 10;

pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let (file_layer, file_status) = match open_run_log() {
        Ok((path, file)) => (
            Some(fmt::layer().with_ansi(false).with_writer(Arc::new(file))),
            Ok(path),
        ),
        Err(error) => (None, Err(error)),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .init();

    match file_status {
        Ok(path) => tracing::info!(file = %path.display(), "file logging enabled"),
        Err(error) => tracing::warn!(error = format!("{error:#}"), "file logging disabled"),
    }
}

fn open_run_log() -> anyhow::Result<(PathBuf, File)> {
    let dirs =
        ProjectDirs::from("io.github", "quik95", APP_NAME).context("no home directory found")?;

    let dir = dirs
        .state_dir()
        .context("platform has no state directory")?;

    fs::create_dir_all(dir)
        .with_context(|| format!("cannot create log directory {}", dir.display()))?;

    prune_old_runs(dir)?;

    let started = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ");
    let path = dir.join(format!("{APP_NAME}.{started}.{}.log", std::process::id()));
    let file = File::create_new(&path)
        .with_context(|| format!("cannot create log file {}", path.display()))?;

    Ok((path, file))
}

fn prune_old_runs(dir: &Path) -> anyhow::Result<()> {
    let mut logs = fs::read_dir(dir)
        .with_context(|| format!("cannot list log directory {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(APP_NAME) && name.ends_with(".log"))
        })
        .collect::<Vec<_>>();
    logs.sort();

    let excess = (logs.len() + 1).saturating_sub(KEPT_RUNS);
    for old in &logs[..excess] {
        fs::remove_file(old)
            .with_context(|| format!("cannot remove old log file {}", old.display()))?;
    }
    Ok(())
}
