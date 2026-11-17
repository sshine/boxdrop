use anyhow::{Context, Result};
use boxdrop_core::{Entry, EntryKind, Fingerprint, TreeManifest};
use boxdrop_fs::{
    ensure_directory, fingerprint as stat_fingerprint, remove_path, scan_path, set_mtime,
    write_file_atomic, write_symlink_atomic,
};
use boxdrop_store::Store;

use crate::scan::Root;

/// The bytes of a locally available entry, from the CAS, falling back to the
/// working tree if the CAS is missing a block.
pub fn blocks_for_entry(
    store: &Store,
    root: &Root,
    path: &str,
    entry: &Entry,
) -> Result<Vec<Vec<u8>>> {
    if !entry.is_file() {
        return Ok(Vec::new());
    }
    let manifest = entry
        .manifest
        .as_ref()
        .context("file entry has no block manifest")?;
    let mut blocks = Vec::with_capacity(manifest.blocks.len());
    let mut missing = false;
    for hash in &manifest.blocks {
        match store.get_chunk(*hash) {
            Ok(bytes) => blocks.push(bytes),
            Err(_) => {
                missing = true;
                break;
            }
        }
    }
    if !missing {
        return Ok(blocks);
    }

    // The index knows the content but the blob store lost it. Re-read the
    // working tree and repair the CAS before giving up.
    let scanned = scan_path(&root.path, path)?
        .with_context(|| format!("file {path} is missing from the working tree"))?;
    if scanned.content_id != entry.content_id {
        anyhow::bail!("working tree content for {path} does not match the index");
    }
    if let (Some(manifest), blocks) = (&scanned.manifest, &scanned.blocks) {
        store.put_content(manifest, blocks)?;
        return Ok(blocks.clone());
    }
    anyhow::bail!("could not recover blocks for {path}")
}

/// Write a file from blocks and verify the manifest before replacing the
/// destination.
pub fn write_file_blocks(
    store: &Store,
    root: &Root,
    path: &str,
    entry: &Entry,
    blocks: &[Vec<u8>],
) -> Result<()> {
    let manifest = entry
        .manifest
        .as_ref()
        .context("file entry has no block manifest")?;
    write_file_atomic(&root.path, path, manifest, blocks, entry.mode)?;
    store.put_content(manifest, blocks)?;
    let absolute = boxdrop_fs::safe_join(&root.path, path)?;
    set_mtime(&absolute, entry.mtime_ns)?;
    let fingerprint = stat_fingerprint(&absolute)?.unwrap_or(Fingerprint {
        size: entry.size,
        mtime_ns: entry.mtime_ns,
        ctime_ns: 0,
        ino: 0,
        mode: entry.mode,
        kind: EntryKind::File,
    });
    store.upsert_file(root.root_id, path, entry, &fingerprint)?;
    Ok(())
}

pub fn apply_entry(
    store: &Store,
    root: &Root,
    path: &str,
    entry: &Entry,
    blocks: &[Vec<u8>],
) -> Result<()> {
    match entry.kind {
        EntryKind::Deleted => {
            remove_path(&root.path, path)?;
            store.upsert_file(root.root_id, path, entry, &Fingerprint::default())?;
        }
        EntryKind::Directory => {
            ensure_directory(&root.path, path)?;
            let absolute = boxdrop_fs::safe_join(&root.path, path)?;
            let fingerprint = stat_fingerprint(&absolute)?.unwrap_or_default();
            store.upsert_file(root.root_id, path, entry, &fingerprint)?;
        }
        EntryKind::Symlink => {
            let target = entry
                .symlink_target
                .as_deref()
                .context("symlink entry has no target")?;
            write_symlink_atomic(&root.path, path, target)?;
            let absolute = boxdrop_fs::safe_join(&root.path, path)?;
            let fingerprint = stat_fingerprint(&absolute)?.unwrap_or_default();
            store.upsert_file(root.root_id, path, entry, &fingerprint)?;
        }
        EntryKind::File => {
            write_file_blocks(store, root, path, entry, blocks)?;
        }
    }
    Ok(())
}

/// Update only the version/vector metadata, used when two sides independently
/// arrived at the same content.
pub fn merge_metadata(store: &Store, root: &Root, path: &str, entry: &Entry) -> Result<()> {
    let existing = store.get_file(root.root_id, path)?;
    let fingerprint = existing
        .as_ref()
        .map(|file| file.fingerprint)
        .unwrap_or_else(|| Fingerprint {
            size: entry.size,
            mtime_ns: entry.mtime_ns,
            ctime_ns: 0,
            ino: 0,
            mode: entry.mode,
            kind: entry.kind,
        });
    store.upsert_file(root.root_id, path, entry, &fingerprint)
}

pub fn manifest_from_store(store: &Store, root: &Root) -> Result<TreeManifest> {
    let mut tree = TreeManifest::new(root.root_id, root.device_id);
    for file in store.list_files(root.root_id)? {
        tree.insert(file.path, file.entry);
    }
    Ok(tree)
}
