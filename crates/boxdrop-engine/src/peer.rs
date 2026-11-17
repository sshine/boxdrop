use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use boxdrop_core::{ContentManifest, Hash32, PROTOCOL_VERSION};
use boxdrop_net::Connection;
use boxdrop_proto::{
    Block, FileBlocks, FileRequest, Hello, Plan, Request, Response, SyncStats as WireStats,
};
use boxdrop_store::Store;

use crate::SyncOptions;
use crate::apply::{apply_entry, blocks_for_entry, manifest_from_store, merge_metadata};
use crate::local::{SyncStats, snapshot};
use crate::reconcile::{ConflictChoice, reconcile};
use crate::scan::{Root, scan_and_index};

pub async fn sync_over_tcp(
    store: &Store,
    root: &Root,
    address: &str,
    _options: &SyncOptions,
) -> Result<SyncStats> {
    let (local, _) = scan_and_index(store, root)?;
    let mut connection = Connection::connect(address).await?;
    connection
        .send(&Request::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            device_id: root.device_id,
            root_id: root.root_id,
            manifest: local,
        }))
        .await?;
    let plan: Plan = match connection.recv::<Response>().await? {
        Response::Plan(plan) => plan,
        Response::Error(error) => bail!("peer refused the session: {error}"),
        other => bail!("expected a plan, got {other:?}"),
    };

    let mut stats = SyncStats::default();

    // Preserve our loser before a pull overwrites the path.
    for conflict in &plan.conflicts {
        if conflict.resolution == ConflictChoice::RemoteWins.as_str() {
            let blocks = blocks_for_entry(store, root, &conflict.path, &conflict.local)?;
            apply_entry(
                store,
                root,
                &conflict.conflict_path,
                &conflict.local,
                &blocks,
            )?;
            stats.conflicts += 1;
        }
    }

    // Ask only for blocks we do not already have.
    let mut requests = Vec::new();
    for pull in &plan.pull {
        if !pull.entry.is_file() {
            continue;
        }
        let manifest = pull
            .entry
            .manifest
            .as_ref()
            .context("pull entry has no manifest")?;
        let missing = manifest
            .blocks
            .iter()
            .filter(|hash| !store.has_chunk(**hash).unwrap_or(false))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            requests.push(FileRequest {
                path: pull.path.clone(),
                content_id: manifest.content_id,
                blocks: missing,
            });
        }
    }

    let mut fetched: HashMap<Hash32, Vec<u8>> = HashMap::new();
    if !requests.is_empty() {
        connection.send(&Request::Fetch(requests)).await?;
        match connection.recv::<Response>().await? {
            Response::Blocks(files) => {
                for file in files {
                    for block in file.blocks {
                        fetched.insert(block.hash, block.data);
                    }
                }
            }
            Response::Error(error) => bail!("peer fetch failed: {error}"),
            other => bail!("expected blocks, got {other:?}"),
        }
    }

    for pull in &plan.pull {
        let blocks = collect_blocks(store, pull.entry.manifest.as_ref(), &fetched)?;
        apply_entry(store, root, &pull.path, &pull.entry, &blocks)?;
        stats.pulled += 1;
        stats.bytes_down += pull.entry.size;
    }
    for merged in &plan.merge {
        merge_metadata(store, root, &merged.path, &merged.entry)?;
    }

    let mut push = Vec::new();
    for planned in &plan.push {
        let blocks = blocks_for_entry(store, root, &planned.path, &planned.entry)?;
        push.push(FileBlocks {
            path: planned.path.clone(),
            entry: planned.entry.clone(),
            blocks: planned
                .entry
                .manifest
                .as_ref()
                .map(|manifest| {
                    manifest
                        .blocks
                        .iter()
                        .copied()
                        .zip(blocks.iter().cloned())
                        .map(|(hash, data)| Block { hash, data })
                        .collect()
                })
                .unwrap_or_default(),
        });
        stats.pushed += 1;
        stats.bytes_up += planned.entry.size;
    }
    connection.send(&Request::Push(push)).await?;
    match connection.recv::<Response>().await? {
        Response::Ack(ack) => {
            stats.conflicts = stats.conflicts.max(ack.conflicts);
        }
        Response::Error(error) => bail!("peer push failed: {error}"),
        other => bail!("expected an ack, got {other:?}"),
    }
    connection.send(&Request::Done).await?;

    let tree = manifest_from_store(store, root)?;
    snapshot(store, root, &tree, Some("peer sync"))?;
    Ok(stats)
}

/// Accept one peer session. The daemon calls this in a loop; the harness calls
/// it once.
pub async fn serve_once(
    store: &Store,
    root: &Root,
    address: &str,
    options: &SyncOptions,
) -> Result<SyncStats> {
    let connection = Connection::accept_once(address).await?;
    serve_connection(store, root, connection, options).await
}

/// Serve one already-accepted connection.
pub async fn serve_connection(
    store: &Store,
    root: &Root,
    mut connection: Connection,
    options: &SyncOptions,
) -> Result<SyncStats> {
    let hello: Hello = match connection.recv::<Request>().await? {
        Request::Hello(hello) => hello,
        other => bail!("expected hello, got {other:?}"),
    };
    if hello.protocol != PROTOCOL_VERSION {
        connection
            .send(&Response::Error(format!(
                "protocol {} is not {}",
                hello.protocol, PROTOCOL_VERSION
            )))
            .await?;
        bail!("unsupported protocol {}", hello.protocol);
    }
    let (server, _) = scan_and_index(store, root)?;
    let plan = reconcile(
        &hello.manifest,
        &server,
        options.conflict_policy,
        boxdrop_store::now_ms(),
    );
    let mut stats = WireStats::default();

    // If the client wins, preserve our copy before accepting its content.
    for conflict in &plan.conflicts {
        if conflict.resolution == ConflictChoice::LocalWins.as_str() {
            let blocks = blocks_for_entry(store, root, &conflict.path, &conflict.remote)?;
            apply_entry(
                store,
                root,
                &conflict.conflict_path,
                &conflict.remote,
                &blocks,
            )?;
            stats.conflicts += 1;
        }
    }

    connection.send(&Response::Plan(plan)).await?;

    loop {
        match connection.recv::<Request>().await? {
            Request::Fetch(requests) => {
                let mut files = Vec::new();
                for request in requests {
                    let entry = store
                        .get_file(root.root_id, &request.path)?
                        .context("fetch for an unknown path")?
                        .entry;
                    if entry.content_id != Some(request.content_id) {
                        connection
                            .send(&Response::Error(format!(
                                "content changed for {}",
                                request.path
                            )))
                            .await?;
                        bail!("content changed during fetch");
                    }
                    let mut blocks = Vec::new();
                    for hash in request.blocks {
                        let data = store
                            .get_chunk(hash)
                            .with_context(|| format!("missing chunk {hash}"))?;
                        blocks.push(Block { hash, data });
                    }
                    files.push(FileBlocks {
                        path: request.path,
                        entry,
                        blocks,
                    });
                }
                connection.send(&Response::Blocks(files)).await?;
            }
            Request::Push(files) => {
                for file in files {
                    let blocks = file
                        .blocks
                        .into_iter()
                        .map(|block| block.data)
                        .collect::<Vec<_>>();
                    apply_entry(store, root, &file.path, &file.entry, &blocks)?;
                    stats.pushed += 1;
                    stats.bytes_up += file.entry.size;
                }
                let tree = manifest_from_store(store, root)?;
                snapshot(store, root, &tree, Some("peer sync"))?;
                connection.send(&Response::Ack(stats.clone())).await?;
            }
            Request::Done => break,
            Request::Hello(_) => bail!("duplicate hello"),
        }
    }

    let tree = manifest_from_store(store, root)?;
    snapshot(store, root, &tree, Some("peer sync"))?;
    Ok(SyncStats {
        pushed: stats.pushed,
        pulled: stats.pulled,
        conflicts: stats.conflicts,
        bytes_up: stats.bytes_up,
        bytes_down: stats.bytes_down,
        scanned_local: 0,
        scanned_remote: 0,
    })
}

fn collect_blocks(
    store: &Store,
    manifest: Option<&ContentManifest>,
    fetched: &HashMap<Hash32, Vec<u8>>,
) -> Result<Vec<Vec<u8>>> {
    let Some(manifest) = manifest else {
        return Ok(Vec::new());
    };
    let mut blocks = Vec::with_capacity(manifest.blocks.len());
    for hash in &manifest.blocks {
        if let Some(bytes) = fetched.get(hash) {
            blocks.push(bytes.clone());
            continue;
        }
        blocks.push(
            store
                .get_chunk(*hash)
                .with_context(|| format!("block {hash} was neither fetched nor local"))?,
        );
    }
    Ok(blocks)
}
