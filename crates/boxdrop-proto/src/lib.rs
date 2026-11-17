//! Peer protocol messages.
//!
//! A session is deliberately small: one manifest exchange, one block fetch, one
//! push. That is the rsync property — one round trip for the common case — while
//! the manifest carries enough information for the receiver to ask only for the
//! blocks it lacks.

use boxdrop_core::{DeviceId, Entry, Hash32, RootId, TreeManifest};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_MAGIC: &[u8; 8] = b"BOXDRP01";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub device_id: DeviceId,
    pub root_id: RootId,
    pub manifest: TreeManifest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedEntry {
    pub path: String,
    pub entry: Entry,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConflictPlan {
    pub path: String,
    pub local: Entry,
    pub remote: Entry,
    pub conflict_path: String,
    pub resolution: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Plan {
    /// Entries the server has that the client should apply.
    pub pull: Vec<PlannedEntry>,
    /// Entries the client has that the server should apply.
    pub push: Vec<PlannedEntry>,
    /// Same content on both sides, but the version vectors need merging.
    pub merge: Vec<PlannedEntry>,
    /// Concurrent edits. `conflict_path` is where the client keeps the remote
    /// copy under the default policy.
    pub conflicts: Vec<ConflictPlan>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub hash: Hash32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRequest {
    pub path: String,
    pub content_id: Hash32,
    pub blocks: Vec<Hash32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileBlocks {
    pub path: String,
    pub entry: Entry,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SyncStats {
    pub pushed: u64,
    pub pulled: u64,
    pub conflicts: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Hello(Hello),
    Fetch(Vec<FileRequest>),
    Push(Vec<FileBlocks>),
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Plan(Plan),
    Blocks(Vec<FileBlocks>),
    Ack(SyncStats),
    Error(String),
}

impl Request {
    pub fn encode(&self) -> Result<Vec<u8>, bincode::error::EncodeError> {
        bincode::serde::encode_to_vec(self, bincode::config::standard())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, bincode::error::DecodeError> {
        bincode::serde::decode_from_slice(bytes, bincode::config::standard())
            .map(|(value, _)| value)
    }
}

impl Response {
    pub fn encode(&self) -> Result<Vec<u8>, bincode::error::EncodeError> {
        bincode::serde::encode_to_vec(self, bincode::config::standard())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, bincode::error::DecodeError> {
        bincode::serde::decode_from_slice(bytes, bincode::config::standard())
            .map(|(value, _)| value)
    }
}
