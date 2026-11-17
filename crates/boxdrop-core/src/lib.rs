//! Domain types shared by every Boxdrop component.
//!
//! The important invariants live here:
//!
//! * a file's identity is a [`FileId`], not its path;
//! * content is addressed by a BLAKE3 [`Hash32`];
//! * concurrency is decided by a [`VersionVector`], never by a timestamp;
//! * a [`Snapshot`] is a content-addressed point in a DAG of trees.

pub mod chunk;
pub mod conflict;
pub mod error;
pub mod hash;
pub mod id;
pub mod manifest;
pub mod snapshot;
pub mod version;

pub use chunk::{ContentManifest, MIN_BLOCK, block_size_for};
pub use conflict::{ConflictPolicy, Resolution};
pub use error::{Error, Result};
pub use hash::{Hash32, hash_bytes};
pub use id::{DeviceId, FileId, RootId};
pub use manifest::{Entry, EntryKind, Fingerprint, TreeManifest};
pub use snapshot::Snapshot;
pub use version::VersionVector;

/// Current wire protocol version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Current on-disk schema version.
pub const SCHEMA_VERSION: u32 = 2;
