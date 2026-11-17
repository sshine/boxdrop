use std::path::Path;

use anyhow::{Context, Result, bail};
use boxdrop_core::{EntryKind, Hash32};
use boxdrop_fs::{ensure_directory, set_mtime, write_file_atomic, write_symlink_atomic};
use boxdrop_store::Store;

/// Materialize a snapshot (or one path inside it) into a directory. This does
/// not touch the index; the caller decides whether the destination is the live
/// root and needs a rescan.
pub fn restore_snapshot(
    store: &Store,
    snapshot_id: Hash32,
    destination: &Path,
    path_filter: Option<&str>,
) -> Result<usize> {
    let entries = store.snapshot_entries(snapshot_id)?;
    if entries.is_empty() {
        bail!("snapshot {snapshot_id} has no entries");
    }
    let mut restored = 0;
    std::fs::create_dir_all(destination)?;
    for (path, entry) in entries {
        if entry.is_deleted() {
            continue;
        }
        if let Some(filter) = path_filter {
            let filter = filter.trim_matches('/');
            if path != filter && !path.starts_with(&format!("{filter}/")) {
                continue;
            }
        }
        match entry.kind {
            EntryKind::Deleted => {}
            EntryKind::Directory => {
                ensure_directory(destination, &path)?;
                restored += 1;
            }
            EntryKind::Symlink => {
                let target = entry
                    .symlink_target
                    .as_deref()
                    .context("symlink entry has no target")?;
                write_symlink_atomic(destination, &path, target)?;
                restored += 1;
            }
            EntryKind::File => {
                let manifest = entry
                    .manifest
                    .as_ref()
                    .context("file entry has no manifest")?;
                let mut blocks = Vec::with_capacity(manifest.blocks.len());
                for hash in &manifest.blocks {
                    blocks.push(
                        store
                            .get_chunk(*hash)
                            .with_context(|| format!("missing chunk {hash}"))?,
                    );
                }
                write_file_atomic(destination, &path, manifest, &blocks, entry.mode)?;
                let absolute = destination.join(&path);
                set_mtime(&absolute, entry.mtime_ns)?;
                restored += 1;
            }
        }
    }
    Ok(restored)
}
