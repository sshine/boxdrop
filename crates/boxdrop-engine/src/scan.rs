use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use boxdrop_core::{DeviceId, Entry, EntryKind, FileId, Fingerprint, RootId, TreeManifest};
use boxdrop_fs::{ScanOptions, scan_root};
use boxdrop_store::Store;

#[derive(Clone, Debug)]
pub struct Root {
    pub root_id: RootId,
    pub device_id: DeviceId,
    pub path: PathBuf,
}

impl Root {
    pub fn new(root_id: RootId, device_id: DeviceId, path: impl Into<PathBuf>) -> Self {
        Self {
            root_id,
            device_id,
            path: path.into(),
        }
    }

    pub fn from_path(store: &Store, path: &Path) -> Result<Self> {
        let device_id = store.ensure_device_id()?;
        let (root_id, _) = match store.root_by_path(path)? {
            Some(found) => found,
            None => {
                let root_id = RootId::new();
                store.put_root(root_id, path, device_id)?;
                (root_id, device_id)
            }
        };
        Ok(Self {
            root_id,
            device_id,
            path: path.to_path_buf(),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct IndexStats {
    pub scanned: u64,
    pub changed: u64,
    pub deleted: u64,
    pub bytes_hashed: u64,
}

/// Walk a root, reuse unchanged metadata, hash changed files, store their
/// chunks, and return a complete tree manifest including tombstones.
pub fn scan_and_index(store: &Store, root: &Root) -> Result<(TreeManifest, IndexStats)> {
    let options = ScanOptions::with_defaults();
    let scanned = scan_root(&root.path, &options)
        .with_context(|| format!("scanning {}", root.path.display()))?;
    let mut existing = store
        .list_files(root.root_id)?
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<std::collections::BTreeMap<_, _>>();

    let mut tree = TreeManifest::new(root.root_id, root.device_id);
    let mut stats = IndexStats::default();

    for item in scanned {
        stats.scanned += 1;
        let old = existing.remove(&item.path);
        let changed = match &old {
            None => true,
            Some(old) => match item.kind {
                EntryKind::File => {
                    old.entry.content_id != item.content_id
                        || old.fingerprint.size != item.fingerprint.size
                        || old.fingerprint.mtime_ns != item.fingerprint.mtime_ns
                        || old.entry.mode != item.mode
                }
                EntryKind::Directory => old.entry.kind != EntryKind::Directory,
                EntryKind::Symlink => {
                    old.entry.kind != EntryKind::Symlink
                        || old.entry.symlink_target != item.symlink_target
                }
                EntryKind::Deleted => true,
            },
        };
        let file_id = old
            .as_ref()
            .map(|old| old.entry.file_id)
            .unwrap_or_else(FileId::new);
        let mut version = old
            .as_ref()
            .map(|old| old.entry.version.clone())
            .unwrap_or_default();
        if changed {
            stats.changed += 1;
            version.increment(root.device_id);
            if item.kind == EntryKind::File {
                if let (Some(manifest), blocks) = (&item.manifest, &item.blocks) {
                    store.put_content(manifest, blocks)?;
                    stats.bytes_hashed += item.size;
                }
            }
        }
        let entry = Entry {
            file_id,
            kind: item.kind,
            size: item.size,
            mtime_ns: item.mtime_ns,
            mode: item.mode,
            content_id: item.content_id,
            manifest: item.manifest,
            version,
            symlink_target: item.symlink_target,
        };
        store.upsert_file(root.root_id, &item.path, &entry, &item.fingerprint)?;
        tree.insert(item.path, entry);
    }

    for (path, old) in existing {
        if old.entry.is_deleted() {
            tree.insert(path, old.entry);
            continue;
        }
        stats.deleted += 1;
        let mut version = old.entry.version;
        version.increment(root.device_id);
        let tombstone = Entry::tombstone(old.entry.file_id, version);
        store.upsert_file(root.root_id, &path, &tombstone, &Fingerprint::default())?;
        tree.insert(path, tombstone);
    }

    Ok((tree, stats))
}
