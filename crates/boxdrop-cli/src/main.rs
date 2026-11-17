use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use boxdrop_core::{ConflictPolicy, Hash32};
use boxdrop_engine::diff::diff_snapshots;
use boxdrop_engine::{
    Root, SyncOptions, init_root, restore_snapshot, scan_and_index, serve_connection, snapshot,
    sync_over_stream, sync_over_tcp,
};
use boxdrop_fs::ScanOptions;
use boxdrop_net::Connection;
use boxdrop_store::{Store, now_ms};
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

const BUS_NAME: &str = "org.boxdrop.Daemon";
const OBJECT_PATH: &str = "/org/boxdrop/Daemon";
const INTERFACE: &str = "org.boxdrop.Daemon";

#[derive(Parser)]
#[command(
    name = "boxdrop",
    version,
    about = "A two-way file synchronizer with versioned history"
)]
struct Cli {
    /// State directory. Defaults to $XDG_STATE_HOME/boxdrop.
    #[arg(long, global = true, env = "BOXDROP_STATE")]
    state: Option<PathBuf>,

    /// Root to operate on. Defaults to the store's first root.
    #[arg(long, global = true)]
    root: Option<PathBuf>,

    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a root and its initial snapshot.
    Init { path: PathBuf },
    /// Show daemon or local status.
    Status,
    /// Sync now through the daemon, or one-shot if it is not running.
    Sync {
        #[arg(long)]
        one_shot: bool,
        /// TCP peer, host:port.
        #[arg(long)]
        peer: Option<String>,
        /// SSH destination, user@host. Runs `boxdrop serve --stdio` remotely.
        #[arg(long)]
        ssh: Option<String>,
        #[arg(long)]
        ssh_port: Option<u16>,
        #[arg(long)]
        remote_root: Option<PathBuf>,
        #[arg(long)]
        remote_state: Option<PathBuf>,
        #[arg(long, default_value = "boxdrop")]
        remote_boxdrop: String,
        #[arg(long, default_value = "conflict-copy")]
        conflict_policy: String,
    },
    /// Serve one peer session over stdio or TCP.
    Serve {
        #[arg(long)]
        stdio: bool,
        #[arg(long)]
        listen: Option<String>,
        #[arg(long, default_value = "conflict-copy")]
        conflict_policy: String,
    },
    /// Pause the daemon.
    Pause,
    /// Resume the daemon.
    Resume,
    /// Take a snapshot now.
    Snapshot {
        #[arg(long, default_value = "manual snapshot")]
        message: String,
    },
    /// List conflict copies.
    Conflicts,
    /// List snapshots.
    Log {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Compare two snapshots.
    Diff { left: String, right: String },
    /// Restore a snapshot, or one path from it, into the root or another directory.
    Restore {
        snapshot: String,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Manage peers for the current root.
    Peer {
        #[command(subcommand)]
        command: PeerCommand,
    },
    /// Inspect or use native filesystem snapshots.
    Native {
        #[command(subcommand)]
        command: NativeCommand,
    },
    /// Check local limits and state.
    Doctor,
}

#[derive(Subcommand)]
enum PeerCommand {
    Add { name: String, address: String },
    List,
    Remove { name: String },
}

#[derive(Subcommand)]
enum NativeCommand {
    /// Show the detected filesystem capabilities.
    Status,
    /// Create a native ZFS or Btrfs snapshot.
    Snapshot { name: String },
    /// List paths changed between two native snapshots.
    Changed { from: String, to: String },
}

#[derive(Serialize)]
struct LocalStatus {
    roots: Vec<String>,
    files: u64,
    queue_pending: u64,
    queue_leased: u64,
    paused: bool,
    snapshots: u64,
    db_bytes: u64,
    blob_bytes: u64,
    chunks: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let cli = Cli::parse();
    let state = cli.state.unwrap_or_else(default_state_dir);

    match cli.command {
        Command::Init { path } => {
            let store = Store::open(&state)?;
            let root = init_root(&store, &path)?;
            println!("initialized {} ({})", path.display(), root.root_id);
        }
        Command::Status => status(&Store::open(&state)?, cli.json).await?,
        Command::Sync {
            one_shot,
            peer,
            ssh,
            ssh_port,
            remote_root,
            remote_state,
            remote_boxdrop,
            conflict_policy,
        } => {
            let policy = parse_policy(&conflict_policy)?;
            if let Some(destination) = ssh {
                let store = Store::open(&state)?;
                let root = resolve_root(&store, cli.root.as_deref())?;
                let remote_root = remote_root.unwrap_or_else(|| root.path.clone());
                ssh_sync(
                    &store,
                    &root,
                    &destination,
                    ssh_port,
                    &remote_root,
                    remote_state.as_deref(),
                    &remote_boxdrop,
                    policy,
                    cli.json,
                )
                .await?;
                return Ok(());
            }
            if !one_shot {
                if let Some(proxy) = proxy().await? {
                    proxy.call::<_, _, ()>("SyncNow", &()).await?;
                    println!("sync requested");
                    return Ok(());
                }
            }
            let store = Store::open(&state)?;
            let root = resolve_root(&store, cli.root.as_deref())?;
            one_shot_sync(&store, &root, peer.as_deref(), policy, cli.json).await?;
        }
        Command::Serve {
            stdio,
            listen,
            conflict_policy,
        } => {
            let policy = parse_policy(&conflict_policy)?;
            let store = Store::open(&state)?;
            let root = resolve_root(&store, cli.root.as_deref())?;
            let options = SyncOptions {
                conflict_policy: policy,
            };
            if stdio {
                let stream = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
                serve_connection(&store, &root, Connection::new(stream), &options).await?;
            } else if let Some(address) = listen {
                let listener = tokio::net::TcpListener::bind(&address).await?;
                eprintln!("serving on {address}");
                loop {
                    let (stream, _) = listener.accept().await?;
                    let connection = Connection::new(stream);
                    serve_connection(&store, &root, connection, &options).await?;
                }
            } else {
                bail!("serve needs --stdio or --listen");
            }
        }
        Command::Pause => {
            if let Some(proxy) = proxy().await? {
                proxy.call::<_, _, ()>("Pause", &()).await?;
                println!("paused");
            } else {
                let store = Store::open(&state)?;
                store.set_paused(true)?;
                println!("paused (no daemon)");
            }
        }
        Command::Resume => {
            if let Some(proxy) = proxy().await? {
                proxy.call::<_, _, ()>("Resume", &()).await?;
                println!("resumed");
            } else {
                let store = Store::open(&state)?;
                store.set_paused(false)?;
                println!("resumed (no daemon)");
            }
        }
        Command::Snapshot { message } => {
            let store = Store::open(&state)?;
            let root = resolve_root(&store, cli.root.as_deref())?;
            let (tree, _) = scan_and_index(&store, &root)?;
            let snapshot = snapshot(&store, &root, &tree, Some(&message))?;
            println!("{}", snapshot.id);
        }
        Command::Conflicts => {
            let store = Store::open(&state)?;
            let root = resolve_root(&store, cli.root.as_deref())?;
            let mut conflicts = ScanOptions::with_defaults();
            conflicts.ignore.clear();
            let entries = boxdrop_fs::scan_root(&root.path, &conflicts)?;
            let mut found = entries
                .into_iter()
                .filter(|entry| entry.path.contains(".boxdrop-conflict-"))
                .map(|entry| entry.path)
                .collect::<Vec<_>>();
            found.sort();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&found)?);
            } else {
                for path in found {
                    println!("{path}");
                }
            }
        }
        Command::Log { limit } => {
            let store = Store::open(&state)?;
            let root = resolve_root(&store, cli.root.as_deref())?;
            let snapshots = store.list_snapshots(root.root_id, limit)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&snapshots)?);
            } else {
                for snapshot in snapshots {
                    println!(
                        "{}  {}  {}",
                        &snapshot.id.to_hex()[..12],
                        snapshot.message.as_deref().unwrap_or(""),
                        snapshot.created_at_ms
                    );
                }
            }
        }
        Command::Diff { left, right } => {
            let store = Store::open(&state)?;
            let left: Hash32 = left.parse()?;
            let right: Hash32 = right.parse()?;
            let diff = diff_snapshots(&store, left, right)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&diff)?);
            } else {
                for entry in diff {
                    println!("{:<8} {}", entry.status, entry.path);
                }
            }
        }
        Command::Restore {
            snapshot: snapshot_text,
            path,
            to,
        } => {
            let store = Store::open(&state)?;
            let root = resolve_root(&store, cli.root.as_deref())?;
            let snapshot_id: Hash32 = snapshot_text.parse()?;
            let destination = to.unwrap_or_else(|| root.path.clone());
            let count = restore_snapshot(&store, snapshot_id, &destination, path.as_deref())?;
            if destination == root.path {
                let (tree, _) = scan_and_index(&store, &root)?;
                snapshot(&store, &root, &tree, Some("restore"))?;
            }
            println!("restored {count} entries into {}", destination.display());
        }
        Command::Peer { command } => {
            let store = Store::open(&state)?;
            let root = resolve_root(&store, cli.root.as_deref())?;
            match command {
                PeerCommand::Add { name, address } => {
                    store.add_peer(root.root_id, &name, &address)?;
                    println!("added peer {name} -> {address}");
                }
                PeerCommand::List => {
                    let peers = store.list_peers(root.root_id)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&peers)?);
                    } else {
                        for peer in peers {
                            println!(
                                "{}\t{}\t{}",
                                peer.name,
                                peer.address,
                                if peer.enabled { "enabled" } else { "disabled" }
                            );
                        }
                    }
                }
                PeerCommand::Remove { name } => {
                    store.remove_peer(root.root_id, &name)?;
                    println!("removed peer {name}");
                }
            }
        }
        Command::Native { command } => {
            let store = Store::open(&state)?;
            let root = resolve_root(&store, cli.root.as_deref())?;
            match command {
                NativeCommand::Status => {
                    let capabilities = boxdrop_fs::detect(&root.path)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&capabilities)?);
                    } else {
                        println!("kind: {:?}", capabilities.kind);
                        println!("filesystem: {}", capabilities.fstype);
                        println!("mount: {}", capabilities.mount_point.display());
                        println!("source: {}", capabilities.source);
                        println!("reflink: {}", capabilities.reflink);
                        if let Some(dataset) = capabilities.zfs_dataset {
                            println!("zfs dataset: {dataset}");
                        }
                        if let Some(subvolume) = capabilities.btrfs_subvolume {
                            println!("btrfs subvolume: {subvolume}");
                        }
                    }
                }
                NativeCommand::Snapshot { name } => {
                    let snapshot = boxdrop_fs::create_snapshot(&root.path, &name)?;
                    println!("{}", snapshot.reference);
                }
                NativeCommand::Changed { from, to } => {
                    let paths = boxdrop_fs::changed_paths(&root.path, &from, &to)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&paths)?);
                    } else {
                        for path in paths {
                            println!("{path}");
                        }
                    }
                }
            }
        }
        Command::Doctor => doctor(&state)?,
    }
    Ok(())
}

async fn status(store: &Store, json_output: bool) -> Result<()> {
    if let Some(proxy) = proxy().await? {
        let value: String = proxy.get_property("StatusJson").await?;
        if json_output {
            println!("{value}");
        } else {
            let status: serde_json::Value = serde_json::from_str(&value)?;
            print_status_value(&status);
        }
        return Ok(());
    }
    let local = local_status(store)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&local)?);
    } else {
        println!("daemon: not running (local state)");
        for root in &local.roots {
            println!("root: {root}");
        }
        println!(
            "files: {}  queue: {} pending / {} leased  paused: {}",
            local.files, local.queue_pending, local.queue_leased, local.paused
        );
        println!(
            "snapshots: {}  chunks: {}  db: {} bytes  blobs: {} bytes",
            local.snapshots, local.chunks, local.db_bytes, local.blob_bytes
        );
    }
    Ok(())
}

fn print_status_value(status: &serde_json::Value) {
    let state = status
        .get("state")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    println!("daemon: {state}");
    if let Some(progress) = status.get("progress").and_then(|value| value.as_u64()) {
        println!("progress: {progress}%");
    }
    if let Some(conflicts) = status.get("conflicts").and_then(|value| value.as_u64()) {
        println!("conflicts: {conflicts}");
    }
    if let Some(up) = status.get("bytes_up").and_then(|value| value.as_u64()) {
        let down = status
            .get("bytes_down")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        println!("traffic: up {up} bytes, down {down} bytes");
    }
    if let Some(pending) = status.get("queue_pending").and_then(|value| value.as_u64()) {
        println!("queue: {pending} pending");
    }
    if let Some(recent) = status.get("recent").and_then(|value| value.as_array()) {
        for item in recent.iter().take(5) {
            let paths = item
                .get("paths")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let at = item
                .get("at_ms")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            let sample = item
                .get("sample")
                .and_then(|value| value.as_array())
                .and_then(|values| values.first())
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if sample.is_empty() {
                println!("recent: {paths} paths at {at}");
            } else {
                println!("recent: {paths} paths at {at} ({sample})");
            }
        }
    }
    if let Some(error) = status.get("error").and_then(|value| value.as_str()) {
        println!("error: {error}");
    }
}

fn local_status(store: &Store) -> Result<LocalStatus> {
    let roots = store.list_roots()?;
    let mut files = 0;
    let mut snapshots = 0;
    for (root_id, _, _) in &roots {
        files += store.file_count(*root_id)?;
        snapshots += store.list_snapshots(*root_id, 100_000)?.len() as u64;
    }
    let queue = store.queue_depth()?;
    let db_bytes = std::fs::metadata(store.state_dir().join("boxdrop.db"))
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    Ok(LocalStatus {
        roots: roots
            .iter()
            .map(|(_, path, _)| path.display().to_string())
            .collect(),
        files,
        queue_pending: queue.pending,
        queue_leased: queue.leased,
        paused: store.paused()?,
        snapshots,
        db_bytes,
        blob_bytes: store.chunk_bytes()?,
        chunks: store.chunk_count()?,
    })
}

async fn one_shot_sync(
    store: &Store,
    root: &Root,
    peer: Option<&str>,
    policy: ConflictPolicy,
    json_output: bool,
) -> Result<()> {
    let started = now_ms();
    let options = SyncOptions {
        conflict_policy: policy,
    };
    if let Some(peer) = peer {
        let stats = sync_over_tcp(store, root, peer, &options).await?;
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "mode": "one-shot",
                    "peer": peer,
                    "pushed": stats.pushed,
                    "pulled": stats.pulled,
                    "conflicts": stats.conflicts,
                    "bytes_up": stats.bytes_up,
                    "bytes_down": stats.bytes_down,
                    "elapsed_ms": now_ms() - started,
                }))?
            );
        } else {
            println!(
                "synced with {peer}: {} pushed, {} pulled, {} conflicts, {} up / {} down bytes in {} ms",
                stats.pushed,
                stats.pulled,
                stats.conflicts,
                stats.bytes_up,
                stats.bytes_down,
                now_ms() - started
            );
        }
    } else {
        let (tree, stats) = scan_and_index(store, root)?;
        let snapshot = snapshot(store, root, &tree, Some("one-shot"))?;
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "mode": "one-shot",
                    "scanned": stats.scanned,
                    "changed": stats.changed,
                    "deleted": stats.deleted,
                    "snapshot": snapshot.id,
                    "elapsed_ms": now_ms() - started,
                }))?
            );
        } else {
            println!(
                "scanned {} paths, {} changed, {} deleted; snapshot {}",
                stats.scanned,
                stats.changed,
                stats.deleted,
                &snapshot.id.to_hex()[..12]
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn ssh_sync(
    store: &Store,
    root: &Root,
    destination: &str,
    port: Option<u16>,
    remote_root: &Path,
    remote_state: Option<&Path>,
    remote_boxdrop: &str,
    policy: ConflictPolicy,
    json_output: bool,
) -> Result<()> {
    validate_ssh_target(destination)?;
    let mut remote = vec![
        remote_boxdrop.to_owned(),
        "serve".to_owned(),
        "--stdio".to_owned(),
        "--root".to_owned(),
        remote_root.to_string_lossy().into_owned(),
    ];
    if let Some(state) = remote_state {
        remote.push("--state".to_owned());
        remote.push(state.to_string_lossy().into_owned());
    }
    remote.push("--conflict-policy".to_owned());
    remote.push(policy.as_str().to_owned());
    let remote_command = remote
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ");

    let mut command = tokio::process::Command::new("ssh");
    command.arg("-T");
    if let Some(port) = port {
        command.args(["-p", &port.to_string()]);
    }
    command
        .arg(destination)
        .arg(remote_command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .with_context(|| format!("starting ssh to {destination}"))?;
    let stdin = child.stdin.take().context("ssh stdin is not piped")?;
    let stdout = child.stdout.take().context("ssh stdout is not piped")?;
    let connection = Connection::new(tokio::io::join(stdout, stdin));
    let started = now_ms();
    let stats = sync_over_stream(
        store,
        root,
        connection,
        &SyncOptions {
            conflict_policy: policy,
        },
    )
    .await?;
    let status = child.wait().await?;
    if !status.success() {
        bail!("ssh exited with {status}");
    }
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "mode": "ssh",
                "destination": destination,
                "pushed": stats.pushed,
                "pulled": stats.pulled,
                "conflicts": stats.conflicts,
                "bytes_up": stats.bytes_up,
                "bytes_down": stats.bytes_down,
                "elapsed_ms": now_ms() - started,
            }))?
        );
    } else {
        println!(
            "synced over ssh with {destination}: {} pushed, {} pulled, {} conflicts in {} ms",
            stats.pushed,
            stats.pulled,
            stats.conflicts,
            now_ms() - started
        );
    }
    Ok(())
}

fn validate_ssh_target(target: &str) -> Result<()> {
    if target.starts_with('-')
        || target.is_empty()
        || target
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        bail!("invalid ssh destination: {target}");
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn parse_policy(value: &str) -> Result<ConflictPolicy> {
    value
        .parse()
        .map_err(|error: String| anyhow::anyhow!(error))
}

fn resolve_root(store: &Store, requested: Option<&Path>) -> Result<Root> {
    if let Some(path) = requested {
        return Root::from_path(store, path);
    }
    let roots = store.list_roots()?;
    let (root_id, path, device_id) = roots
        .into_iter()
        .next()
        .context("no root configured; run `boxdrop init <path>`")?;
    Ok(Root::new(root_id, device_id, path))
}

fn doctor(state: &Path) -> Result<()> {
    let store = Store::open(state)?;
    let local = local_status(&store)?;
    let watches = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .unwrap_or_else(|_| "unknown".to_owned());
    let instances = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_instances")
        .unwrap_or_else(|_| "unknown".to_owned());
    println!("state: {}", state.display());
    println!("inotify max_user_watches: {}", watches.trim());
    println!("inotify max_user_instances: {}", instances.trim());
    println!("roots: {}", local.roots.len());
    println!("files: {}", local.files);
    println!(
        "metadata: db {} bytes, blobs {} bytes, chunks {}",
        local.db_bytes, local.blob_bytes, local.chunks
    );
    if let Some(root) = local.roots.first() {
        if let Ok(capabilities) = boxdrop_fs::detect(Path::new(root)) {
            println!(
                "native: {:?} ({}, reflink={})",
                capabilities.kind, capabilities.fstype, capabilities.reflink
            );
        }
    }
    Ok(())
}

async fn proxy() -> Result<Option<zbus::Proxy<'static>>> {
    let connection = match zbus::Connection::session().await {
        Ok(connection) => connection,
        Err(_) => return Ok(None),
    };
    let bus = zbus::fdo::DBusProxy::new(&connection).await?;
    let name: zbus::names::BusName<'_> = BUS_NAME
        .try_into()
        .map_err(|error| anyhow::anyhow!("invalid bus name: {error}"))?;
    if !bus.name_has_owner(name).await? {
        return Ok(None);
    }
    match zbus::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE).await {
        Ok(proxy) => Ok(Some(proxy)),
        Err(_) => Ok(None),
    }
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
