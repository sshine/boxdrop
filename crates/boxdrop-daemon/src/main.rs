use std::path::PathBuf;

use anyhow::Result;
use boxdrop_daemon::{Config, run};
use clap::Parser;

#[derive(Parser)]
#[command(name = "boxdropd", version, about = "The Boxdrop background daemon")]
struct Cli {
    /// Directory to synchronize.
    #[arg(long, env = "BOXDROP_ROOT")]
    root: PathBuf,

    /// State directory (database, blobs, socket).
    #[arg(long, env = "BOXDROP_STATE")]
    state: Option<PathBuf>,

    /// Peer to synchronize with, as host:port.
    #[arg(long, env = "BOXDROP_PEER")]
    peer: Option<String>,

    /// Listen for peer sessions on host:port.
    #[arg(long, env = "BOXDROP_LISTEN")]
    listen: Option<String>,

    /// Explicit path to the watch helper.
    #[arg(long, env = "BOXDROP_WATCHER")]
    watcher: Option<PathBuf>,

    #[arg(long, default_value_t = 300)]
    base_quiet_ms: i64,

    #[arg(long, default_value_t = 30_000)]
    max_quiet_ms: i64,

    #[arg(long, default_value_t = 64)]
    batch_size: u32,

    #[arg(long, default_value_t = 30_000)]
    lease_ms: i64,

    #[arg(long, default_value_t = 60)]
    rescan_secs: u64,

    #[arg(long, default_value = "conflict-copy")]
    conflict_policy: String,

    #[arg(long)]
    no_dbus: bool,

    #[arg(long)]
    no_watch: bool,

    /// Scan once, flush the queue once, and exit.
    #[arg(long)]
    once: bool,

    /// Stop after this many seconds. Used by integration tests.
    #[arg(long)]
    run_for_secs: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let state = cli.state.unwrap_or_else(default_state_dir);
    let config = Config {
        root: cli.root,
        state,
        peer: cli.peer,
        listen: cli.listen,
        watcher: cli.watcher,
        base_quiet_ms: cli.base_quiet_ms,
        max_quiet_ms: cli.max_quiet_ms,
        batch_size: cli.batch_size,
        lease_ms: cli.lease_ms,
        rescan_secs: cli.rescan_secs,
        no_dbus: cli.no_dbus,
        no_watch: cli.no_watch,
        once: cli.once,
        run_for_secs: cli.run_for_secs,
        conflict_policy: cli
            .conflict_policy
            .parse()
            .map_err(|error: String| anyhow::anyhow!(error))?,
    };
    run(config).await
}

fn default_state_dir() -> PathBuf {
    if let Ok(value) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(value).join("boxdrop");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/state/boxdrop");
    }
    PathBuf::from(".boxdrop")
}
