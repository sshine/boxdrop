#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use boxdrop_engine::{SyncOptions, init_root, sync_local};
use boxdrop_store::Store;
use tempfile::TempDir;

struct Host {
    _dir: TempDir,
    store: Store,
    root: boxdrop_engine::Root,
    path: std::path::PathBuf,
}

fn host() -> Host {
    let dir = TempDir::new().unwrap();
    let root_path = dir.path().join("root");
    let store = Store::open(dir.path().join("state")).unwrap();
    let root = init_root(&store, &root_path).unwrap();
    Host {
        _dir: dir,
        store,
        root,
        path: root_path,
    }
}

fn sync(a: &Host, b: &Host) {
    sync_local(
        &a.store,
        &a.root,
        &b.store,
        &b.root,
        &SyncOptions::default(),
    )
    .unwrap();
}

#[test]
fn creates_propagate_both_ways() {
    let a = host();
    let b = host();
    fs::write(a.path.join("from-a.txt"), b"alpha").unwrap();
    fs::create_dir_all(b.path.join("dir")).unwrap();
    fs::write(b.path.join("dir/from-b.txt"), b"beta").unwrap();
    sync(&a, &b);
    assert_eq!(fs::read(a.path.join("dir/from-b.txt")).unwrap(), b"beta");
    assert_eq!(fs::read(b.path.join("from-a.txt")).unwrap(), b"alpha");
}

#[test]
fn deletes_propagate() {
    let a = host();
    let b = host();
    fs::write(a.path.join("gone.txt"), b"delete me").unwrap();
    sync(&a, &b);
    assert!(b.path.join("gone.txt").is_file());
    fs::remove_file(a.path.join("gone.txt")).unwrap();
    sync(&a, &b);
    assert!(!b.path.join("gone.txt").exists());
}

#[test]
fn edits_propagate_without_conflict() {
    let a = host();
    let b = host();
    fs::write(a.path.join("f"), b"one").unwrap();
    sync(&a, &b);
    fs::write(a.path.join("f"), b"two").unwrap();
    sync(&a, &b);
    assert_eq!(fs::read(b.path.join("f")).unwrap(), b"two");
}

#[test]
fn concurrent_edits_keep_both_versions() {
    let a = host();
    let b = host();
    fs::write(a.path.join("f"), b"base").unwrap();
    sync(&a, &b);

    fs::write(a.path.join("f"), b"from a, a much newer edit").unwrap();
    fs::write(b.path.join("f"), b"from b").unwrap();
    sync(&a, &b);

    let entries = fs::read_dir(&a.path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let conflicts = entries
        .iter()
        .filter(|name| name.contains("boxdrop-conflict"))
        .count();
    assert_eq!(conflicts, 1, "expected one conflict copy, got {entries:?}");
    let contents = entries
        .iter()
        .filter(|name| name.starts_with('f'))
        .map(|name| fs::read(a.path.join(name)).unwrap())
        .collect::<Vec<_>>();
    assert!(
        contents
            .iter()
            .any(|value| value == b"from a, a much newer edit")
    );
    assert!(contents.iter().any(|value| value == b"from b"));
}

#[test]
fn modified_listener_catches_up() {
    let a = host();
    let b = host();
    fs::write(a.path.join("f"), b"first").unwrap();
    sync(&a, &b);
    fs::write(b.path.join("f"), b"second").unwrap();
    sync(&a, &b);
    assert_eq!(fs::read(a.path.join("f")).unwrap(), b"second");
}

#[test]
fn snapshot_history_records_each_sync() {
    let a = host();
    let b = host();
    fs::write(a.path.join("f"), b"one").unwrap();
    sync(&a, &b);
    fs::write(a.path.join("f"), b"two").unwrap();
    sync(&a, &b);
    let snapshots = a.store.list_snapshots(a.root.root_id, 10).unwrap();
    assert!(snapshots.len() >= 2);
}
