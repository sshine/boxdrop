use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};

use crate::normalize_relative;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchKind {
    Touched,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchEvent {
    pub path: String,
    pub kind: WatchKind,
}

/// Owns a recursive watcher. Dropping it stops the watch.
pub struct Watcher {
    _inner: RecommendedWatcher,
}

impl Watcher {
    pub fn start(root: &Path, sender: Sender<WatchEvent>) -> Result<Self> {
        let root = root.to_path_buf();
        let watched_root = root.clone();
        let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            let event = match result {
                Ok(event) => event,
                Err(error) => {
                    tracing::warn!("filesystem watch error: {error}");
                    return;
                }
            };
            for path in event.paths {
                let relative = match path.strip_prefix(&watched_root) {
                    Ok(path) if path.as_os_str().is_empty() => continue,
                    Ok(path) => match normalize_relative(path) {
                        Ok(relative) => relative,
                        Err(_) => continue,
                    },
                    Err(_) => continue,
                };
                if relative.split('/').any(|component| {
                    matches!(
                        component,
                        ".boxdrop" | ".boxdrop-tmp" | "target" | "node_modules" | ".git"
                    )
                }) {
                    continue;
                }
                let kind = match event.kind {
                    EventKind::Remove(_) => WatchKind::Removed,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any => {
                        WatchKind::Touched
                    }
                    EventKind::Access(_) | EventKind::Other => continue,
                };
                let _ = sender.send(WatchEvent {
                    path: relative,
                    kind,
                });
            }
        })
        .context("creating filesystem watcher")?;
        watcher
            .watch(root.as_path(), RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", root.display()))?;
        Ok(Self { _inner: watcher })
    }
}

/// A cheap, portable recursive watch for tests and the harness. On Linux this
/// uses inotify; on other platforms notify's best backend.
pub fn watch_root(
    root: impl Into<PathBuf>,
) -> Result<(Watcher, std::sync::mpsc::Receiver<WatchEvent>)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let watcher = Watcher::start(&root.into(), sender)?;
    Ok((watcher, receiver))
}
