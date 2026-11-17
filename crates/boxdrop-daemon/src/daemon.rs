use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use boxdrop_core::ConflictPolicy;
use boxdrop_core::TreeManifest;
use boxdrop_engine::{
    Root, SyncOptions, SyncStats, init_root, refresh_paths, scan_and_index, serve_connection,
    snapshot, sync_over_tcp,
};
use boxdrop_store::{Store, now_ms};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::dbus::{DbusDaemon, serve};
use crate::status::{RecentSync, Status};
use crate::watch::WatchMessage;

#[derive(Clone, Debug)]
pub struct Config {
    pub root: PathBuf,
    pub state: PathBuf,
    pub peer: Option<String>,
    pub listen: Option<String>,
    pub watcher: Option<PathBuf>,
    pub base_quiet_ms: i64,
    pub max_quiet_ms: i64,
    pub batch_size: u32,
    pub lease_ms: i64,
    pub rescan_secs: u64,
    pub no_dbus: bool,
    pub no_watch: bool,
    pub once: bool,
    pub run_for_secs: Option<u64>,
    pub conflict_policy: ConflictPolicy,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            state: PathBuf::new(),
            peer: None,
            listen: None,
            watcher: None,
            base_quiet_ms: 300,
            max_quiet_ms: 30_000,
            batch_size: 64,
            lease_ms: 30_000,
            rescan_secs: 60,
            no_dbus: false,
            no_watch: false,
            once: false,
            run_for_secs: None,
            conflict_policy: ConflictPolicy::ConflictCopy,
        }
    }
}

pub async fn run(config: Config) -> Result<()> {
    std::fs::create_dir_all(&config.state)?;
    std::fs::create_dir_all(&config.root)?;
    let store = Arc::new(Store::open(&config.state)?);
    let root = Arc::new(init_root(&store, &config.root)?);
    let status = Arc::new(Mutex::new(Status::default()));
    let sync_now = Arc::new(AtomicBool::new(false));
    let rescan = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));

    {
        let mut status = status.lock().unwrap_or_else(|p| p.into_inner());
        status.file_count = store.file_count(root.root_id)?;
        status.root = root.path.display().to_string();
        status.paused = store.paused()?;
        if status.paused {
            status.state = "paused".to_owned();
        }
    }
    store.reset_expired_leases(now_ms())?;
    let options = SyncOptions {
        conflict_policy: config.conflict_policy,
    };
    let mut peers = store
        .list_peers(root.root_id)?
        .into_iter()
        .filter(|peer| peer.enabled)
        .map(|peer| peer.address)
        .collect::<Vec<_>>();
    if let Some(peer) = config.peer.clone() {
        if !peers.contains(&peer) {
            peers.push(peer);
        }
    }

    let mut watcher_child = None;
    if !config.no_watch {
        let socket = config.state.join("watch.sock");
        let _ = std::fs::remove_file(&socket);
        match std::os::unix::net::UnixListener::bind(&socket) {
            Ok(listener) => {
                let store = Arc::clone(&store);
                let root = Arc::clone(&root);
                let watcher_root = Arc::clone(&root);
                let rescan = Arc::clone(&rescan);
                let status = Arc::clone(&status);
                let base = config.base_quiet_ms;
                let max = config.max_quiet_ms;
                std::thread::Builder::new()
                    .name("boxdrop-watch-accept".to_owned())
                    .spawn(move || {
                        accept_watch(listener, store, watcher_root, rescan, status, base, max);
                    })?;
                watcher_child = spawn_watcher(&config, &root.path, &socket);
            }
            Err(error) => warn!("could not create watch socket: {error}"),
        }
    }

    if let Some(address) = config.listen.clone() {
        let store = Arc::clone(&store);
        let root = Arc::clone(&root);
        let status = Arc::clone(&status);
        let options = options.clone();
        tokio::spawn(async move {
            match TcpListener::bind(&address).await {
                Ok(listener) => {
                    info!("peer listener on {address}");
                    loop {
                        match listener.accept().await {
                            Ok((stream, _)) => {
                                let connection = boxdrop_net::Connection::new(stream);
                                match serve_connection(&store, &root, connection, &options).await {
                                    Ok(stats) => {
                                        let mut status =
                                            status.lock().unwrap_or_else(|p| p.into_inner());
                                        status.bytes_up += stats.bytes_up;
                                        status.bytes_down += stats.bytes_down;
                                        status.conflicts += stats.conflicts;
                                    }
                                    Err(error) => warn!("peer session failed: {error:#}"),
                                }
                            }
                            Err(error) => {
                                warn!("peer accept failed: {error}");
                                break;
                            }
                        }
                    }
                }
                Err(error) => warn!("could not bind peer listener {address}: {error}"),
            }
        });
    }

    if !config.no_dbus {
        let daemon = DbusDaemon {
            status: Arc::clone(&status),
            store: Arc::clone(&store),
            sync_now: Arc::clone(&sync_now),
        };
        std::thread::Builder::new()
            .name("boxdrop-dbus".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        warn!("could not start D-Bus runtime: {error}");
                        return;
                    }
                };
                if let Err(error) = runtime.block_on(serve(daemon)) {
                    warn!("D-Bus service unavailable: {error:#}");
                }
            })?;
    }

    if config.once {
        full_sync(&store, &root, &peers, &options, &status).await?;
        info!("one-shot sync complete");
        return Ok(());
    }

    {
        let shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                shutdown.store(true, Ordering::Relaxed);
            }
        });
    }
    if let Some(seconds) = config.run_for_secs {
        let shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            shutdown.store(true, Ordering::Relaxed);
        });
    }

    let mut last_rescan = std::time::Instant::now();
    while !shutdown.load(Ordering::Relaxed) {
        {
            let depth = store.queue_depth()?;
            let mut status = status.lock().unwrap_or_else(|p| p.into_inner());
            status.queue_pending = depth.pending;
            status.queue_leased = depth.leased;
            status.file_count = store.file_count(root.root_id)?;
            status.paused = store.paused()?;
        }

        if store.paused()? {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }

        if sync_now.swap(false, Ordering::Relaxed) {
            if let Err(error) = full_sync(&store, &root, &peers, &options, &status).await {
                set_error(&status, error);
            }
        }

        if rescan.swap(false, Ordering::Relaxed)
            || last_rescan.elapsed() >= Duration::from_secs(config.rescan_secs.max(1))
        {
            last_rescan = std::time::Instant::now();
            if let Err(error) = full_sync(&store, &root, &peers, &options, &status).await {
                set_error(&status, error);
            }
        }

        let items = store.claim_due(now_ms(), config.batch_size, config.lease_ms)?;
        if items.is_empty() {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        let paths = items
            .iter()
            .map(|item| item.path.clone())
            .collect::<Vec<_>>();
        let result = process_batch(&store, &root, &paths, &peers, &options, &status).await;
        match result {
            Ok(()) => {
                for item in &items {
                    store.complete_change(item.id)?;
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                for item in &items {
                    store.fail_change(item.id, now_ms(), &message)?;
                }
                set_error(&status, error);
            }
        }
    }

    if let Some(mut child) = watcher_child {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = std::fs::remove_file(config.state.join("watch.sock"));
    Ok(())
}

async fn process_batch(
    store: &Store,
    root: &Root,
    paths: &[String],
    peers: &[String],
    options: &SyncOptions,
    status: &Arc<Mutex<Status>>,
) -> Result<()> {
    {
        let mut status = status.lock().unwrap_or_else(|p| p.into_inner());
        status.state = "scanning".to_owned();
        status.progress = 5;
        status.error = None;
    }
    let (tree, stats) = refresh_paths(store, root, paths)?;
    {
        let mut status = status.lock().unwrap_or_else(|p| p.into_inner());
        status.state = "syncing".to_owned();
        status.progress = 25;
    }
    let sync_stats = sync_peers(store, root, peers, options, &tree, "queued change").await?;
    let mut status = status.lock().unwrap_or_else(|p| p.into_inner());
    status.state = "resting".to_owned();
    status.progress = 100;
    status.last_sync_ms = now_ms();
    status.file_count = store.file_count(root.root_id)?;
    if let Some(sync_stats) = sync_stats {
        status.bytes_up += sync_stats.bytes_up;
        status.bytes_down += sync_stats.bytes_down;
        status.conflicts += sync_stats.conflicts;
        push_recent(
            &mut status,
            RecentSync {
                at_ms: now_ms(),
                paths: paths.len() as u64,
                sample: paths.iter().take(5).cloned().collect(),
                bytes_up: sync_stats.bytes_up,
                bytes_down: sync_stats.bytes_down,
                conflicts: sync_stats.conflicts,
            },
        );
    } else {
        push_recent(
            &mut status,
            RecentSync {
                at_ms: now_ms(),
                paths: paths.len() as u64,
                sample: paths.iter().take(5).cloned().collect(),
                bytes_up: 0,
                bytes_down: 0,
                conflicts: 0,
            },
        );
    }
    let _ = stats;
    Ok(())
}

async fn full_sync(
    store: &Store,
    root: &Root,
    peers: &[String],
    options: &SyncOptions,
    status: &Arc<Mutex<Status>>,
) -> Result<()> {
    {
        let mut status = status.lock().unwrap_or_else(|p| p.into_inner());
        status.state = "scanning".to_owned();
        status.progress = 5;
        status.error = None;
    }
    let (tree, _) = scan_and_index(store, root)?;
    {
        let mut status = status.lock().unwrap_or_else(|p| p.into_inner());
        status.state = "syncing".to_owned();
        status.progress = 40;
    }
    let sync_stats = sync_peers(store, root, peers, options, &tree, "scan").await?;
    let mut status = status.lock().unwrap_or_else(|p| p.into_inner());
    status.state = "resting".to_owned();
    status.progress = 100;
    status.last_sync_ms = now_ms();
    status.file_count = store.file_count(root.root_id)?;
    if let Some(sync_stats) = sync_stats {
        status.bytes_up += sync_stats.bytes_up;
        status.bytes_down += sync_stats.bytes_down;
        status.conflicts += sync_stats.conflicts;
        push_recent(
            &mut status,
            RecentSync {
                at_ms: now_ms(),
                paths: 0,
                sample: Vec::new(),
                bytes_up: sync_stats.bytes_up,
                bytes_down: sync_stats.bytes_down,
                conflicts: sync_stats.conflicts,
            },
        );
    }
    Ok(())
}

fn set_error(status: &Arc<Mutex<Status>>, error: anyhow::Error) {
    let mut status = status.lock().unwrap_or_else(|p| p.into_inner());
    status.state = "error".to_owned();
    status.error = Some(format!("{error:#}"));
    warn!("sync error: {error:#}");
}

fn push_recent(status: &mut Status, recent: RecentSync) {
    status.recent.insert(0, recent);
    status.recent.truncate(8);
}

fn spawn_watcher(
    config: &Config,
    root: &std::path::Path,
    socket: &std::path::Path,
) -> Option<Child> {
    let watcher = config
        .watcher
        .clone()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.join("boxdrop-watch")))
        })
        .unwrap_or_else(|| PathBuf::from("boxdrop-watch"));
    if !watcher.exists() && config.watcher.is_none() {
        warn!(
            "watch helper not found at {}; falling back to periodic scans",
            watcher.display()
        );
        return None;
    }
    match Command::new(&watcher)
        .args([
            "--root",
            root.to_string_lossy().as_ref(),
            "--socket",
            socket.to_string_lossy().as_ref(),
        ])
        .spawn()
    {
        Ok(child) => Some(child),
        Err(error) => {
            warn!("could not start watcher {}: {error}", watcher.display());
            None
        }
    }
}

fn accept_watch(
    listener: std::os::unix::net::UnixListener,
    store: Arc<Store>,
    root: Arc<Root>,
    rescan: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    base_quiet_ms: i64,
    max_quiet_ms: i64,
) {
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                warn!("watch socket accept failed: {error}");
                continue;
            }
        };
        let store = Arc::clone(&store);
        let root = Arc::clone(&root);
        let rescan = Arc::clone(&rescan);
        let status = Arc::clone(&status);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let message: WatchMessage = match serde_json::from_str(line.trim()) {
                            Ok(message) => message,
                            Err(error) => {
                                warn!("bad watcher message: {error}");
                                continue;
                            }
                        };
                        match message.kind.as_str() {
                            "rescan" => rescan.store(true, Ordering::Relaxed),
                            "touched" | "removed" => {
                                if let Some(path) = message.path {
                                    match store.enqueue_change(
                                        root.root_id,
                                        &path,
                                        now_ms(),
                                        base_quiet_ms,
                                        max_quiet_ms,
                                    ) {
                                        Ok(_) => {
                                            let mut status =
                                                status.lock().unwrap_or_else(|p| p.into_inner());
                                            status.queue_pending += 1;
                                        }
                                        Err(error) => warn!("enqueue failed: {error:#}"),
                                    }
                                }
                            }
                            other => warn!("unknown watcher message kind {other}"),
                        }
                    }
                    Err(error) => {
                        warn!("watch socket read failed: {error}");
                        break;
                    }
                }
            }
        });
    }
}

async fn sync_peers(
    store: &Store,
    root: &Root,
    peers: &[String],
    options: &SyncOptions,
    tree: &TreeManifest,
    message: &str,
) -> Result<Option<SyncStats>> {
    if peers.is_empty() {
        snapshot(store, root, tree, Some(message))?;
        return Ok(None);
    }
    let mut aggregate = SyncStats::default();
    let mut succeeded = false;
    let mut last_error = None;
    for peer in peers {
        match sync_over_tcp(store, root, peer, options).await {
            Ok(stats) => {
                succeeded = true;
                aggregate.pushed += stats.pushed;
                aggregate.pulled += stats.pulled;
                aggregate.conflicts += stats.conflicts;
                aggregate.bytes_up += stats.bytes_up;
                aggregate.bytes_down += stats.bytes_down;
            }
            Err(error) => {
                warn!("peer {peer} failed: {error:#}");
                last_error = Some(error);
            }
        }
    }
    if succeeded {
        Ok(Some(aggregate))
    } else if let Some(error) = last_error {
        Err(error)
    } else {
        Ok(None)
    }
}
