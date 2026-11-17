#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

use boxdrop_store::Store;
use tempfile::TempDir;

#[test]
fn once_scans_and_snapshots() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    let state = dir.path().join("state");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("hello.txt"), b"hello").unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_boxdropd"))
        .args([
            "--root",
            root.to_string_lossy().as_ref(),
            "--state",
            state.to_string_lossy().as_ref(),
            "--once",
            "--no-dbus",
            "--no-watch",
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let store = Store::open(&state).unwrap();
    let roots = store.list_roots().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(store.file_count(roots[0].0).unwrap(), 1);
    assert!(!store.list_snapshots(roots[0].0, 10).unwrap().is_empty());
}

#[test]
fn watcher_drains_a_change_into_a_snapshot() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    let state = dir.path().join("state");
    fs::create_dir_all(&root).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_boxdropd"))
        .args([
            "--root",
            root.to_string_lossy().as_ref(),
            "--state",
            state.to_string_lossy().as_ref(),
            "--no-dbus",
            "--watcher",
            env!("CARGO_BIN_EXE_boxdrop-watch"),
            "--run-for-secs",
            "4",
            "--rescan-secs",
            "3600",
            "--base-quiet-ms",
            "40",
            "--max-quiet-ms",
            "80",
        ])
        .spawn()
        .unwrap();

    let socket = state.join("watch.sock");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "watch socket was not created");

    fs::write(root.join("watched.txt"), b"from the watcher").unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = false;
    while Instant::now() < deadline {
        if let Ok(store) = Store::open(&state) {
            if let Ok(roots) = store.list_roots() {
                if let Some((root_id, _, _)) = roots.first() {
                    if store.file_count(*root_id).unwrap_or(0) == 1
                        && store.list_snapshots(*root_id, 10).unwrap_or_default().len() >= 2
                    {
                        found = true;
                        break;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(found, "the watcher's change never reached a snapshot");
}

#[test]
fn queue_survives_a_restart_with_expired_leases() {
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("state");
    let store = Store::open(&state).unwrap();
    let root_id = boxdrop_core::RootId::new();
    store
        .put_root(
            root_id,
            dir.path().join("root").as_path(),
            boxdrop_core::DeviceId::new(),
        )
        .unwrap();
    let item = store.enqueue_change(root_id, "f", 0, 0, 0).unwrap();
    store.claim_due(0, 1, 10).unwrap();
    assert_eq!(store.reset_expired_leases(1_000).unwrap(), 1);
    assert_eq!(store.due_changes(1_000, 10).unwrap()[0].id, item.id);
}
