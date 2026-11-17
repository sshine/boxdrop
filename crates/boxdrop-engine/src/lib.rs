//! Reconciliation, indexing and application.
//!
//! The engine is deliberately separate from the daemon and the CLI so the same
//! code can run in a one-shot process, a long-lived daemon, or the deterministic
//! simulator.

mod apply;
mod local;
mod peer;
mod reconcile;
mod scan;

pub use apply::{apply_entry, blocks_for_entry, manifest_from_store};
pub use local::{SyncStats, init_root, snapshot, sync_local};
pub use peer::{serve_connection, serve_once, sync_over_tcp};
pub use reconcile::{ConflictChoice, reconcile};
pub use scan::{IndexStats, Root, refresh_paths, scan_and_index};

use boxdrop_core::ConflictPolicy;

#[derive(Clone, Debug)]
pub struct SyncOptions {
    pub conflict_policy: ConflictPolicy,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            conflict_policy: ConflictPolicy::ConflictCopy,
        }
    }
}
