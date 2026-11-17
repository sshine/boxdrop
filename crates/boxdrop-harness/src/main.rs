//! Boxdrop's test harness.
//!
//! `sim` is a fast, seeded, in-process convergence test. `run` starts two
//! separate processes in user/mount namespaces, feeds them random files, and
//! verifies that the pair converges. `soak` is `run` with a long duration and a
//! pruner so the working set stays inside a disk budget.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use anyhow::{Context, Result, bail};
use boxdrop_core::{Hash32, TreeManifest};
use boxdrop_engine::{Root, SyncOptions, init_root, serve_connection, sync_local, sync_over_tcp};
use boxdrop_fs::{ScanOptions, scan_root};
use boxdrop_store::Store;
use clap::{Args, Parser, Subcommand};
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(version, about = "Boxdrop simulation and soak harness")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    /// Fast seeded convergence tests in one process.
    Sim(SimArgs),
    /// Two real processes, isolated with Linux user and mount namespaces.
    Run(RunArgs),
    /// Long-running bounded-disk workload; a specialisation of `run`.
    Soak(SoakArgs),
    /// Compare two trees by content hash.
    Verify(VerifyArgs),
    /// Internal child mode used by `run`.
    Host(HostArgs),
}

#[derive(Args)]
struct SimArgs {
    #[arg(long, default_value_t = 10)]
    seeds: u64,
    #[arg(long, default_value_t = 80)]
    steps: u64,
}

#[derive(Args)]
struct RunArgs {
    #[arg(long, default_value_t = 8)]
    duration_secs: u64,
    #[arg(long, default_value_t = 20.0)]
    rate: f64,
    #[arg(long, default_value_t = 64)]
    high_water_mib: u64,
    #[arg(long, default_value_t = 32)]
    low_water_mib: u64,
    /// Keep the default user/mount namespace isolation.
    #[arg(long)]
    no_userns: bool,
    #[arg(long)]
    keep: bool,
}

#[derive(Args)]
struct SoakArgs {
    #[arg(long, default_value_t = 600)]
    duration_secs: u64,
    #[arg(long, default_value_t = 20.0)]
    rate: f64,
    #[arg(long, default_value_t = 512)]
    high_water_mib: u64,
    #[arg(long, default_value_t = 256)]
    low_water_mib: u64,
    #[arg(long)]
    no_userns: bool,
    #[arg(long)]
    keep: bool,
}

#[derive(Args)]
struct VerifyArgs {
    left: PathBuf,
    right: PathBuf,
}

#[derive(Args)]
struct HostArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    state: PathBuf,
    #[arg(long)]
    prefix: String,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    #[arg(long, default_value_t = 8)]
    duration_secs: u64,
    #[arg(long)]
    ingest_secs: Option<u64>,
    #[arg(long, default_value_t = 20.0)]
    rate: f64,
    #[arg(long, default_value_t = 64)]
    high_water_mib: u64,
    #[arg(long, default_value_t = 32)]
    low_water_mib: u64,
    #[arg(long)]
    listen: Option<String>,
    #[arg(long)]
    peer: Option<String>,
    #[arg(long, default_value_t = 500)]
    sync_interval_ms: u64,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    match Cli::parse().command {
        CommandKind::Sim(args) => sim(args),
        CommandKind::Run(args) => run(args, false),
        CommandKind::Soak(args) => run(
            RunArgs {
                duration_secs: args.duration_secs,
                rate: args.rate,
                high_water_mib: args.high_water_mib,
                low_water_mib: args.low_water_mib,
                no_userns: args.no_userns,
                keep: args.keep,
            },
            true,
        ),
        CommandKind::Verify(args) => verify(&args.left, &args.right),
        CommandKind::Host(args) => run_host(args),
    }
}

fn sim(args: SimArgs) -> Result<()> {
    for seed in 0..args.seeds {
        let dir = tempfile::TempDir::new()?;
        let root_a = dir.path().join("a");
        let root_b = dir.path().join("b");
        let store_a = Store::open(dir.path().join("state-a"))?;
        let store_b = Store::open(dir.path().join("state-b"))?;
        let a = init_root(&store_a, &root_a)?;
        let b = init_root(&store_b, &root_b)?;
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let mut operations = 0u64;

        for step in 0..args.steps {
            let side = rng.random_range(0..2);
            let side_name = if side == 0 { "a" } else { "b" };
            let shared = rng.random_bool(0.2);
            let prefix = if shared {
                "shared".to_owned()
            } else {
                side_name.to_owned()
            };
            let name = format!("{prefix}/f{}.bin", rng.random_range(0..8));
            let absolute = if side == 0 {
                root_a.join(&name)
            } else {
                root_b.join(&name)
            };
            apply_random_op(&mut rng, &absolute)?;
            operations += 1;

            if step % 4 == 3 {
                sync_local(&store_a, &a, &store_b, &b, &SyncOptions::default())
                    .with_context(|| format!("seed {seed}, step {step}"))?;
                let left = hashes(&root_a)?;
                let right = hashes(&root_b)?;
                if left != right {
                    let only_left = left
                        .keys()
                        .filter(|key| !right.contains_key(*key))
                        .cloned()
                        .collect::<Vec<_>>();
                    let only_right = right
                        .keys()
                        .filter(|key| !left.contains_key(*key))
                        .cloned()
                        .collect::<Vec<_>>();
                    bail!(
                        "seed {seed}, step {step}: trees diverged; only left {only_left:?}, only right {only_right:?}"
                    );
                }
            }
        }
        sync_local(&store_a, &a, &store_b, &b, &SyncOptions::default())?;
        let left = hashes(&root_a)?;
        let right = hashes(&root_b)?;
        if left != right {
            bail!("seed {seed}: final trees diverged");
        }
        eprintln!(
            "seed {seed}: converged after {operations} operations, {} files, metadata db {} bytes",
            left.len(),
            fs::metadata(dir.path().join("state-a/boxdrop.db"))
                .map(|meta| meta.len())
                .unwrap_or(0)
        );
    }
    println!("sim: {} seeds converged", args.seeds);
    Ok(())
}

fn apply_random_op(rng: &mut ChaCha20Rng, path: &Path) -> Result<()> {
    match rng.random_range(0..5) {
        0 | 1 => {
            let size = if rng.random_bool(0.02) {
                rng.random_range(100_000..1_000_000)
            } else {
                rng.random_range(0..4096)
            };
            let mut bytes = vec![0u8; size];
            rng.fill(&mut bytes[..]);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, bytes)?;
        }
        2 => {
            use std::io::Write;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            let bytes = vec![b'x'; rng.random_range(0..2048)];
            file.write_all(&bytes)?;
        }
        3 => {
            let _ = fs::remove_file(path);
        }
        _ => {
            let target = path.with_file_name(format!("renamed-{}.bin", rng.random_range(0..8)));
            let _ = fs::rename(path, target);
        }
    }
    Ok(())
}

fn run(args: RunArgs, soak: bool) -> Result<()> {
    let dir = tempfile::TempDir::new()?;
    let root_a = dir.path().join("host-a/root");
    let root_b = dir.path().join("host-b/root");
    fs::create_dir_all(&root_a)?;
    fs::create_dir_all(&root_b)?;
    let port = free_port()?;
    let address = format!("127.0.0.1:{port}");
    let exe = std::env::current_exe()?;

    let server = spawn_host(
        &exe,
        &root_a,
        &dir.path().join("host-a/state"),
        "host-a",
        1,
        Some(&address),
        None,
        &args,
        args.no_userns,
        args.duration_secs + 300,
    )?;
    std::thread::sleep(Duration::from_millis(250));
    let mut client = spawn_host(
        &exe,
        &root_b,
        &dir.path().join("host-b/state"),
        "host-b",
        2,
        None,
        Some(&address),
        &args,
        args.no_userns,
        args.duration_secs,
    )?;

    let mut server = server;
    let client_status = client.wait()?;
    if !client_status.success() {
        let _ = server.kill();
        bail!("client host exited with {client_status}");
    }

    // The client has stopped writing; make one final exchange while the
    // server is still listening, then stop the server.
    {
        let store = Store::open(dir.path().join("host-b/state"))?;
        let root = Root::from_path(&store, &root_b)?;
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async {
            sync_over_tcp(&store, &root, &address, &SyncOptions::default())
                .await
                .context("final harness sync")?;
            Ok::<_, anyhow::Error>(())
        })?;
    }
    let _ = server.kill();
    let _ = server.wait();
    verify(&root_a, &root_b)?;
    let stats = metadata_report(dir.path())?;
    println!(
        "run: {} converged; duration {}s; {}; roots {} and {}",
        if soak { "soak" } else { "harness" },
        args.duration_secs,
        stats,
        root_a.display(),
        root_b.display()
    );
    if args.keep {
        let keep = dir.keep();
        println!("kept {}", keep.display());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_host(
    exe: &Path,
    root: &Path,
    state: &Path,
    prefix: &str,
    seed: u64,
    listen: Option<&str>,
    peer: Option<&str>,
    args: &RunArgs,
    no_userns: bool,
    duration_secs: u64,
) -> Result<Child> {
    let mut command = if no_userns {
        let mut command = Command::new(exe);
        command.arg("host");
        command
    } else {
        let mut command = Command::new("unshare");
        command.args([
            "--user",
            "--map-root-user",
            "--mount",
            "--pid",
            "--fork",
            exe.to_string_lossy().as_ref(),
            "host",
        ]);
        command
    };
    command
        .args([
            "--root",
            root.to_string_lossy().as_ref(),
            "--state",
            state.to_string_lossy().as_ref(),
            "--prefix",
            prefix,
            "--seed",
            &seed.to_string(),
            "--duration-secs",
            &duration_secs.to_string(),
            "--ingest-secs",
            &args.duration_secs.to_string(),
            "--rate",
            &args.rate.to_string(),
            "--high-water-mib",
            &args.high_water_mib.to_string(),
            "--low-water-mib",
            &args.low_water_mib.to_string(),
        ])
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    if let Some(listen) = listen {
        command.args(["--listen", listen]);
    }
    if let Some(peer) = peer {
        command.args(["--peer", peer]);
    }
    command.spawn().context("spawning harness host")
}

fn run_host(args: HostArgs) -> Result<()> {
    fs::create_dir_all(&args.root)?;
    fs::create_dir_all(&args.state)?;
    let store = Arc::new(Store::open(&args.state)?);
    let root = Arc::new(init_root(&store, &args.root)?);
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        let duration = Duration::from_secs(args.duration_secs);
        std::thread::spawn(move || {
            std::thread::sleep(duration);
            stop.store(true, Ordering::Relaxed);
        });
    }

    let ingester = {
        let stop = Arc::clone(&stop);
        let root = Arc::clone(&root);
        let prefix = args.prefix.clone();
        let rate = args.rate;
        let seed = args.seed;
        let ingest_duration = Duration::from_secs(args.ingest_secs.unwrap_or(args.duration_secs));
        std::thread::spawn(move || ingest(&root, &prefix, seed, rate, ingest_duration, stop))
    };
    let pruner = if args.high_water_mib > 0 {
        let stop = Arc::clone(&stop);
        let path = root.path.clone();
        Some(std::thread::spawn(move || {
            prune_loop(
                &path,
                args.high_water_mib * 1024 * 1024,
                args.low_water_mib * 1024 * 1024,
                stop,
            )
        }))
    } else {
        None
    };

    let runtime = tokio::runtime::Runtime::new()?;
    if let Some(address) = args.listen.clone() {
        runtime.block_on(async {
            let listener = TcpListener::bind(&address).await?;
            eprintln!("host {} listening on {address}", args.prefix);
            while !stop.load(Ordering::Relaxed) {
                match tokio::time::timeout(Duration::from_millis(250), listener.accept()).await {
                    Ok(Ok((stream, _))) => {
                        let connection = boxdrop_net::Connection::new(stream);
                        if let Err(error) =
                            serve_connection(&store, &root, connection, &SyncOptions::default())
                                .await
                        {
                            eprintln!("host {} session failed: {error:#}", args.prefix);
                        }
                    }
                    Ok(Err(error)) => return Err(error.into()),
                    Err(_) => {}
                }
            }
            Ok::<_, anyhow::Error>(())
        })?;
    } else if let Some(peer) = args.peer.clone() {
        runtime.block_on(async {
            let interval = Duration::from_millis(args.sync_interval_ms.max(50));
            while !stop.load(Ordering::Relaxed) {
                if let Err(error) =
                    sync_over_tcp(&store, &root, &peer, &SyncOptions::default()).await
                {
                    eprintln!("host {} sync attempt: {error:#}", args.prefix);
                }
                tokio::time::sleep(interval).await;
            }
        });
    } else {
        std::thread::sleep(Duration::from_secs(args.duration_secs));
    }

    stop.store(true, Ordering::Relaxed);
    let _ = ingester.join();
    if let Some(pruner) = pruner {
        let _ = pruner.join();
    }
    if args.peer.is_some() {
        std::thread::sleep(Duration::from_millis(500));
        runtime.block_on(async {
            if let Err(error) = sync_over_tcp(
                &store,
                &root,
                args.peer.as_deref().unwrap_or(""),
                &SyncOptions::default(),
            )
            .await
            {
                eprintln!("host {} final sync failed: {error:#}", args.prefix);
            }
        });
    }
    let tree = boxdrop_engine::manifest_from_store(&store, &root)?;
    let report = host_report(&store, &root, &tree)?;
    println!("{report}");
    Ok(())
}

fn ingest(
    root: &Root,
    prefix: &str,
    seed: u64,
    rate: f64,
    duration: Duration,
    stop: Arc<AtomicBool>,
) {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let interval = if rate > 0.0 {
        Duration::from_secs_f64(1.0 / rate)
    } else {
        Duration::from_millis(100)
    };
    let started = std::time::Instant::now();
    let mut counter = 0u64;
    while !stop.load(Ordering::Relaxed) && started.elapsed() < duration {
        let name = format!("{prefix}/dir{}/file-{counter}.bin", rng.random_range(0..4));
        let path = root.path.join(&name);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let size = if rng.random_bool(0.03) {
            rng.random_range(256 * 1024..2 * 1024 * 1024)
        } else {
            rng.random_range(0..16 * 1024)
        };
        let mut bytes = vec![0u8; size];
        rng.fill(&mut bytes[..]);
        let _ = fs::write(&path, bytes);
        counter += 1;
        std::thread::sleep(interval);
    }
}

fn prune_loop(root: &Path, high: u64, low: u64, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        if let Ok(used) = allocated_bytes(root) {
            if used > high {
                if let Ok(files) = oldest_files(root) {
                    let mut remaining = used;
                    for (size, path) in files {
                        if remaining <= low {
                            break;
                        }
                        if fs::remove_file(&path).is_ok() {
                            remaining = remaining.saturating_sub(size);
                        }
                    }
                    eprintln!(
                        "pruned {} to {} bytes",
                        root.display(),
                        allocated_bytes(root).unwrap_or(0)
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn allocated_bytes(root: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                total += entry.metadata()?.blocks() * 512;
            }
            #[cfg(not(unix))]
            {
                total += entry.metadata()?.len();
            }
        }
    }
    Ok(total)
}

fn oldest_files(root: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            #[cfg(unix)]
            let size = entry.metadata()?.blocks() * 512;
            #[cfg(not(unix))]
            let size = entry.metadata()?.len();
            let modified = entry
                .metadata()?
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            files.push((modified, size, entry.path().to_path_buf()));
        }
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    Ok(files
        .into_iter()
        .map(|(_, size, path)| (size, path))
        .collect())
}

fn host_report(store: &Store, root: &Root, tree: &TreeManifest) -> Result<String> {
    let db = fs::metadata(store.state_dir().join("boxdrop.db"))
        .map(|meta| meta.len())
        .unwrap_or(0);
    let blobs = store.chunk_bytes()?;
    Ok(format!(
        "host-report root={} files={} bytes={} chunks={} chunk-bytes={} db-bytes={} snapshots={}",
        root.path.display(),
        tree.file_count(),
        tree.total_bytes(),
        store.chunk_count()?,
        blobs,
        db,
        store.list_snapshots(root.root_id, 10_000)?.len()
    ))
}

fn metadata_report(dir: &Path) -> Result<String> {
    let mut dbs = 0u64;
    let mut blobs = 0u64;
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let name = entry.file_name().to_string_lossy();
            if name == "boxdrop.db" || name.starts_with("boxdrop.db-") {
                dbs += entry.metadata()?.len();
            } else if entry.path().components().any(|c| c.as_os_str() == "blobs") {
                blobs += entry.metadata()?.len();
            }
        }
    }
    Ok(format!("metadata db {dbs} bytes, blobs {blobs} bytes"))
}

fn hashes(root: &Path) -> Result<BTreeMap<String, Option<Hash32>>> {
    let mut map = BTreeMap::new();
    for entry in scan_root(root, &ScanOptions::with_defaults())? {
        map.insert(entry.path, entry.content_id);
    }
    Ok(map)
}

fn verify(left: &Path, right: &Path) -> Result<()> {
    let left_hashes = hashes(left)?;
    let right_hashes = hashes(right)?;
    if left_hashes != right_hashes {
        let only_left = left_hashes
            .keys()
            .filter(|key| !right_hashes.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        let only_right = right_hashes
            .keys()
            .filter(|key| !left_hashes.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        let changed = left_hashes
            .iter()
            .filter(|(key, value)| right_hashes.get(*key).is_some_and(|other| other != *value))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        bail!(
            "verification failed: only left {only_left:?}, only right {only_right:?}, changed {changed:?}"
        );
    }
    println!("verify: {} paths match", left_hashes.len());
    Ok(())
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}
