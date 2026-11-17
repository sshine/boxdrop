use anyhow::{Context, Result};
use boxdrop_core::TreeManifest;
use boxdrop_store::Store;

use crate::SyncOptions;
use crate::apply::{apply_entry, blocks_for_entry, manifest_from_store, merge_metadata};
use crate::reconcile::reconcile;
use crate::scan::{Root, scan_and_index};

#[derive(Clone, Debug, Default)]
pub struct SyncStats {
    pub pushed: u64,
    pub pulled: u64,
    pub conflicts: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub scanned_local: u64,
    pub scanned_remote: u64,
}

pub fn init_root(store: &Store, path: &std::path::Path) -> Result<Root> {
    std::fs::create_dir_all(path)?;
    let root = Root::from_path(store, path)?;
    let (tree, _) = scan_and_index(store, &root)?;
    snapshot(store, &root, &tree, Some("initial snapshot"))?;
    Ok(root)
}

pub fn sync_local(
    store_a: &Store,
    root_a: &Root,
    store_b: &Store,
    root_b: &Root,
    options: &SyncOptions,
) -> Result<SyncStats> {
    let (local, local_stats) = scan_and_index(store_a, root_a)?;
    let (remote, remote_stats) = scan_and_index(store_b, root_b)?;
    let plan = reconcile(
        &local,
        &remote,
        options.conflict_policy,
        boxdrop_store::now_ms(),
    );
    let mut stats = SyncStats {
        scanned_local: local_stats.scanned,
        scanned_remote: remote_stats.scanned,
        ..SyncStats::default()
    };

    // Prepare the loser copies. Local-wins conflicts are preserved on the
    // server (B) and pulled back by A; remote-wins conflicts are preserved on
    // the client (A) and pushed to B. Both sides therefore end up with both
    // versions after one session.
    for conflict in &plan.conflicts {
        stats.conflicts += 1;
        match conflict.resolution.as_str() {
            "local-wins" => {
                let blocks = blocks_for_entry(store_b, root_b, &conflict.path, &conflict.remote)?;
                apply_entry(
                    store_b,
                    root_b,
                    &conflict.conflict_path,
                    &conflict.remote,
                    &blocks,
                )?;
            }
            "remote-wins" => {
                let blocks = blocks_for_entry(store_a, root_a, &conflict.path, &conflict.local)?;
                apply_entry(
                    store_a,
                    root_a,
                    &conflict.conflict_path,
                    &conflict.local,
                    &blocks,
                )?;
            }
            _ => {}
        }
    }

    for pull in &plan.pull {
        let blocks = blocks_for_entry(store_b, root_b, &pull.path, &pull.entry)?;
        apply_entry(store_a, root_a, &pull.path, &pull.entry, &blocks)?;
        stats.pulled += 1;
        stats.bytes_down += pull.entry.size;
    }
    for push in &plan.push {
        let blocks = blocks_for_entry(store_a, root_a, &push.path, &push.entry)?;
        apply_entry(store_b, root_b, &push.path, &push.entry, &blocks)?;
        stats.pushed += 1;
        stats.bytes_up += push.entry.size;
    }

    for merged in &plan.merge {
        merge_metadata(store_a, root_a, &merged.path, &merged.entry)?;
        merge_metadata(store_b, root_b, &merged.path, &merged.entry)?;
    }

    let local_after = manifest_from_store(store_a, root_a)?;
    let remote_after = manifest_from_store(store_b, root_b)?;
    snapshot(store_a, root_a, &local_after, Some("sync"))?;
    snapshot(store_b, root_b, &remote_after, Some("sync"))?;

    Ok(stats)
}

pub fn snapshot(
    store: &Store,
    root: &Root,
    tree: &TreeManifest,
    message: Option<&str>,
) -> Result<boxdrop_core::Snapshot> {
    let latest = store.list_snapshots(root.root_id, 1)?;
    let tree_hash = tree.tree_hash();
    // A periodic rescan of an unchanged tree must not grow the history. Only a
    // new tree earns a new snapshot.
    if let Some(latest) = latest.first() {
        if latest.tree_hash == tree_hash {
            return Ok(latest.clone());
        }
    }
    let parents = latest
        .first()
        .map(|snapshot| snapshot.id)
        .into_iter()
        .collect::<Vec<_>>();
    store
        .create_snapshot(tree, &parents, message)
        .with_context(|| format!("creating snapshot for {}", root.path.display()))
}
