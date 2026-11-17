use serde::{Deserialize, Serialize};

use crate::hash::Hash32;
use crate::id::{DeviceId, RootId};

/// A git-like point in the history of a root. The tree itself lives in the
/// store as a content-addressed tree manifest; this is the commit object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: Hash32,
    pub parents: Vec<Hash32>,
    pub root_id: RootId,
    pub tree_hash: Hash32,
    pub device_id: DeviceId,
    pub created_at_ms: i64,
    pub message: Option<String>,
}

impl Snapshot {
    pub fn compute_id(
        parents: &[Hash32],
        root_id: RootId,
        tree_hash: Hash32,
        device_id: DeviceId,
        created_at_ms: i64,
        message: Option<&str>,
    ) -> Hash32 {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"boxdrop-snapshot-v1");
        for parent in parents {
            hasher.update(parent.as_bytes());
        }
        hasher.update(root_id.uuid().as_bytes());
        hasher.update(tree_hash.as_bytes());
        hasher.update(device_id.uuid().as_bytes());
        hasher.update(&created_at_ms.to_le_bytes());
        if let Some(message) = message {
            hasher.update(message.as_bytes());
        }
        Hash32::new(*hasher.finalize().as_bytes())
    }
}
