#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::net::TcpListener;
use std::sync::Arc;

use boxdrop_engine::{SyncOptions, init_root, serve_once, sync_over_tcp};
use boxdrop_store::Store;
use tempfile::TempDir;

struct Host {
    _dir: TempDir,
    store: Arc<Store>,
    root: boxdrop_engine::Root,
    path: std::path::PathBuf,
}

fn host() -> Host {
    let dir = TempDir::new().unwrap();
    let root_path = dir.path().join("root");
    let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
    let root = init_root(&store, &root_path).unwrap();
    Host {
        _dir: dir,
        store,
        root,
        path: root_path,
    }
}

async fn sync(a: &Host, b: &Host) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let server = tokio::spawn({
        let store = Arc::clone(&b.store);
        let root = b.root.clone();
        async move {
            serve_once(&store, &root, &address.to_string(), &SyncOptions::default())
                .await
                .unwrap();
        }
    });
    let mut last = None;
    for _ in 0..50 {
        match sync_over_tcp(
            &a.store,
            &a.root,
            &address.to_string(),
            &SyncOptions::default(),
        )
        .await
        {
            Ok(_) => {
                last = None;
                break;
            }
            Err(error) => {
                last = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
    }
    if let Some(error) = last {
        panic!("client did not connect: {error:#}");
    }
    server.await.unwrap();
}

#[tokio::test]
async fn tcp_sync_moves_files_both_ways() {
    let a = host();
    let b = host();
    fs::write(a.path.join("from-a"), b"alpha").unwrap();
    fs::write(b.path.join("from-b"), b"beta").unwrap();
    sync(&a, &b).await;
    assert_eq!(fs::read(a.path.join("from-b")).unwrap(), b"beta");
    assert_eq!(fs::read(b.path.join("from-a")).unwrap(), b"alpha");
}
