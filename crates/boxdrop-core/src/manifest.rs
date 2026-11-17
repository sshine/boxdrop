use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::chunk::ContentManifest;
use crate::hash::{Hash32, hash_bytes};
use crate::id::{DeviceId, FileId, RootId};
use crate::version::VersionVector;

/// The local stat fields used to avoid hashing an unchanged file. These are
/// local hints, not part of the wire manifest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub ino: u64,
    pub mode: u32,
    pub kind: EntryKind,
}

impl Fingerprint {
    pub fn from_metadata(metadata: &std::fs::Metadata, kind: EntryKind) -> Self {
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as i64)
            .unwrap_or(0);
        let ctime_ns = metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as i64)
            .unwrap_or(0);
        #[cfg(unix)]
        let (ino, mode) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.ino(), metadata.mode())
        };
        #[cfg(not(unix))]
        let (ino, mode) = (0, 0);
        Self {
            size: metadata.len(),
            mtime_ns,
            ctime_ns,
            ino,
            mode,
            kind,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    #[default]
    File,
    Directory,
    Symlink,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub file_id: FileId,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ns: i64,
    pub mode: u32,
    pub content_id: Option<Hash32>,
    pub manifest: Option<ContentManifest>,
    pub version: VersionVector,
    pub symlink_target: Option<String>,
}

impl Entry {
    pub fn is_deleted(&self) -> bool {
        self.kind == EntryKind::Deleted
    }

    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File
    }

    pub fn is_directory(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    pub fn is_symlink(&self) -> bool {
        self.kind == EntryKind::Symlink
    }

    pub fn same_content(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.content_id == other.content_id
            && self.symlink_target == other.symlink_target
    }

    pub fn tombstone(file_id: FileId, version: VersionVector) -> Self {
        Self {
            file_id,
            kind: EntryKind::Deleted,
            size: 0,
            mtime_ns: 0,
            mode: 0,
            content_id: None,
            manifest: None,
            version,
            symlink_target: None,
        }
    }
}

/// A complete view of a root at one point in time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeManifest {
    pub root_id: RootId,
    pub device_id: DeviceId,
    pub entries: BTreeMap<String, Entry>,
}

impl TreeManifest {
    pub fn new(root_id: RootId, device_id: DeviceId) -> Self {
        Self {
            root_id,
            device_id,
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, path: impl Into<String>, entry: Entry) {
        self.entries.insert(path.into(), entry);
    }

    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.get(path)
    }

    /// A stable digest of the tree, used as the snapshot's tree id.
    pub fn tree_hash(&self) -> Hash32 {
        let canonical = serde_json::to_vec(&self.entries).unwrap_or_default();
        hash_bytes(canonical)
    }

    pub fn live_entries(&self) -> impl Iterator<Item = (&String, &Entry)> {
        self.entries.iter().filter(|(_, entry)| !entry.is_deleted())
    }

    pub fn file_count(&self) -> usize {
        self.live_entries().filter(|(_, e)| e.is_file()).count()
    }

    pub fn total_bytes(&self) -> u64 {
        self.live_entries().map(|(_, e)| e.size).sum()
    }
}
