use std::collections::BTreeMap;

use anyhow::Result;
use boxdrop_core::{Entry, Hash32};
use boxdrop_store::Store;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffEntry {
    pub path: String,
    pub status: String,
}

pub fn diff_snapshots(store: &Store, left: Hash32, right: Hash32) -> Result<Vec<DiffEntry>> {
    let left_entries = live_entries(store, left)?;
    let right_entries = live_entries(store, right)?;
    let mut paths = left_entries
        .keys()
        .chain(right_entries.keys())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let mut diff = Vec::new();
    for path in paths {
        match (left_entries.get(&path), right_entries.get(&path)) {
            (None, Some(_)) => diff.push(DiffEntry {
                path,
                status: "added".to_owned(),
            }),
            (Some(_), None) => diff.push(DiffEntry {
                path,
                status: "deleted".to_owned(),
            }),
            (Some(left), Some(right)) if !same_visible(left, right) => diff.push(DiffEntry {
                path,
                status: "modified".to_owned(),
            }),
            _ => {}
        }
    }
    Ok(diff)
}

fn same_visible(left: &Entry, right: &Entry) -> bool {
    left.kind == right.kind
        && left.content_id == right.content_id
        && left.symlink_target == right.symlink_target
        && left.mode == right.mode
}

fn live_entries(store: &Store, snapshot: Hash32) -> Result<BTreeMap<String, Entry>> {
    let mut entries = BTreeMap::new();
    for (path, entry) in store.snapshot_entries(snapshot)? {
        if !entry.is_deleted() {
            entries.insert(path, entry);
        }
    }
    Ok(entries)
}
