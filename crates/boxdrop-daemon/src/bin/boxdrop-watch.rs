//! A deliberately stateless watcher.
//!
//! It owns an inotify watch and a socket, and nothing else. Every event is
//! forwarded to the daemon as a JSON line. The daemon supervises it; if the
//! socket goes away the watcher exits, and the daemon either restarts it or
//! falls back to a periodic scan.

use std::io::{BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use boxdrop_daemon::WatchMessage;
use boxdrop_fs::{WatchKind, watch_root};
use clap::Parser;
use serde_json::to_string;

#[derive(Parser)]
#[command(
    version,
    about = "Watch a Boxdrop root and forward events to the daemon"
)]
struct Cli {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    socket: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let (_watcher, receiver) =
        watch_root(&cli.root).with_context(|| format!("watching {}", cli.root.display()))?;
    let stream = connect_with_retry(&cli.socket)?;
    let mut writer = BufWriter::new(stream);
    write_message(&mut writer, &WatchMessage::rescan())?;
    while let Ok(event) = receiver.recv() {
        let message = match event.kind {
            WatchKind::Touched => WatchMessage::touched(event.path),
            WatchKind::Removed => WatchMessage::removed(event.path),
        };
        if write_message(&mut writer, &message).is_err() {
            break;
        }
    }
    Ok(())
}

fn connect_with_retry(socket: &Path) -> Result<UnixStream> {
    let mut attempts = 0;
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(error) if attempts < 20 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(100));
                let _ = error;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_message(writer: &mut BufWriter<UnixStream>, message: &WatchMessage) -> Result<()> {
    let line = to_string(message)?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
