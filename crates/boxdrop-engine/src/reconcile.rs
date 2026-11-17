use boxdrop_core::{ConflictPolicy, Entry, TreeManifest};
use boxdrop_proto::{ConflictPlan, Plan, PlannedEntry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictChoice {
    LocalWins,
    RemoteWins,
}

impl ConflictChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalWins => "local-wins",
            Self::RemoteWins => "remote-wins",
        }
    }
}

/// Compare two trees and produce the transfer plan.
///
/// The decision order is intentional:
/// 1. identical content is not a transfer, even if version vectors differ;
/// 2. a dominating version fast-forwards without a conflict;
/// 3. only genuinely concurrent edits consult the conflict policy;
/// 4. an edit beats a concurrent deletion.
pub fn reconcile(
    local: &TreeManifest,
    remote: &TreeManifest,
    policy: ConflictPolicy,
    now_ms: i64,
) -> Plan {
    let mut plan = Plan::default();
    let mut paths = local
        .entries
        .keys()
        .chain(remote.entries.keys())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();

    for path in paths {
        let left = local.get(&path);
        let right = remote.get(&path);
        match (left, right) {
            (None, None) => {}
            (Some(local_entry), None) => {
                if !local_entry.is_deleted() {
                    plan.push.push(PlannedEntry {
                        path,
                        entry: local_entry.clone(),
                    });
                }
            }
            (None, Some(remote_entry)) => {
                if !remote_entry.is_deleted() {
                    plan.pull.push(PlannedEntry {
                        path,
                        entry: remote_entry.clone(),
                    });
                }
            }
            (Some(local_entry), Some(remote_entry)) => {
                if local_entry.same_content(remote_entry) {
                    if local_entry.version != remote_entry.version {
                        let mut merged = local_entry.clone();
                        merged.version.merge(&remote_entry.version);
                        plan.merge.push(PlannedEntry {
                            path,
                            entry: merged,
                        });
                    }
                    continue;
                }
                if local_entry.version.dominates(&remote_entry.version) {
                    plan.push.push(PlannedEntry {
                        path,
                        entry: local_entry.clone(),
                    });
                    continue;
                }
                if remote_entry.version.dominates(&local_entry.version) {
                    plan.pull.push(PlannedEntry {
                        path,
                        entry: remote_entry.clone(),
                    });
                    continue;
                }

                // Genuinely concurrent. A concurrent edit beats a deletion; two
                // concurrent deletions are a no-op.
                if local_entry.is_deleted() && remote_entry.is_deleted() {
                    continue;
                }
                if local_entry.is_deleted() {
                    plan.pull.push(PlannedEntry {
                        path: path.clone(),
                        entry: remote_entry.clone(),
                    });
                    continue;
                }
                if remote_entry.is_deleted() {
                    plan.push.push(PlannedEntry {
                        path: path.clone(),
                        entry: local_entry.clone(),
                    });
                    continue;
                }

                match policy {
                    ConflictPolicy::Manual => {
                        plan.conflicts.push(ConflictPlan {
                            conflict_path: conflict_path(
                                &path,
                                remote.device_id.short().as_str(),
                                now_ms,
                            ),
                            path,
                            local: local_entry.clone(),
                            remote: remote_entry.clone(),
                            resolution: "manual".to_owned(),
                        });
                    }
                    ConflictPolicy::LastWriteWins => {
                        if local_wins(local_entry, remote_entry) {
                            plan.push.push(PlannedEntry {
                                path,
                                entry: local_entry.clone(),
                            });
                        } else {
                            plan.pull.push(PlannedEntry {
                                path,
                                entry: remote_entry.clone(),
                            });
                        }
                    }
                    ConflictPolicy::ConflictCopy | ConflictPolicy::MergeText => {
                        // M3 implements conflict copies. MergeText deliberately
                        // falls back to a copy until the three-way merger lands.
                        let choice = if local_wins(local_entry, remote_entry) {
                            ConflictChoice::LocalWins
                        } else {
                            ConflictChoice::RemoteWins
                        };
                        let conflict_path =
                            conflict_path(&path, remote.device_id.short().as_str(), now_ms);
                        plan.conflicts.push(ConflictPlan {
                            conflict_path: conflict_path.clone(),
                            path: path.clone(),
                            local: local_entry.clone(),
                            remote: remote_entry.clone(),
                            resolution: match choice {
                                ConflictChoice::LocalWins => "local-wins".to_owned(),
                                ConflictChoice::RemoteWins => "remote-wins".to_owned(),
                            },
                        });
                        match choice {
                            // The winner takes the original path. The loser
                            // becomes an ordinary file at the conflict path, and
                            // the plan carries it in both directions so a single
                            // session converges both hosts.
                            ConflictChoice::LocalWins => {
                                plan.push.push(PlannedEntry {
                                    path,
                                    entry: local_entry.clone(),
                                });
                                plan.pull.push(PlannedEntry {
                                    path: conflict_path,
                                    entry: remote_entry.clone(),
                                });
                            }
                            ConflictChoice::RemoteWins => {
                                plan.pull.push(PlannedEntry {
                                    path,
                                    entry: remote_entry.clone(),
                                });
                                plan.push.push(PlannedEntry {
                                    path: conflict_path,
                                    entry: local_entry.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    plan
}

pub fn local_wins(local: &Entry, remote: &Entry) -> bool {
    (local.mtime_ns, local.file_id.uuid()) >= (remote.mtime_ns, remote.file_id.uuid())
}

pub fn conflict_path(path: &str, device: &str, now_ms: i64) -> String {
    format!("{path}.boxdrop-conflict-{device}-{now_ms}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use boxdrop_core::{ContentManifest, DeviceId, EntryKind, FileId, RootId, VersionVector};

    fn file(content: &[u8], version: VersionVector, mtime: i64) -> Entry {
        let (manifest, _) = ContentManifest::from_bytes(content);
        Entry {
            file_id: FileId::new(),
            kind: EntryKind::File,
            size: content.len() as u64,
            mtime_ns: mtime,
            mode: 0o644,
            content_id: Some(manifest.content_id),
            manifest: Some(manifest),
            version,
            symlink_target: None,
        }
    }

    fn trees(local: Entry, remote: Entry) -> (TreeManifest, TreeManifest) {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let mut left = TreeManifest::new(RootId::new(), a);
        let mut right = TreeManifest::new(RootId::new(), b);
        left.insert("f", local);
        right.insert("f", remote);
        (left, right)
    }

    #[test]
    fn equal_content_merges_versions_without_transfer() {
        let mut left_version = VersionVector::new();
        let a = DeviceId::new();
        left_version.increment(a);
        let mut right_version = VersionVector::new();
        right_version.increment(DeviceId::new());
        let (left, right) = trees(
            file(b"same", left_version, 1),
            file(b"same", right_version, 2),
        );
        let plan = reconcile(&left, &right, ConflictPolicy::ConflictCopy, 10);
        assert!(plan.pull.is_empty());
        assert!(plan.push.is_empty());
        assert_eq!(plan.merge.len(), 1);
    }

    #[test]
    fn newer_version_fast_forwards() {
        let a = DeviceId::new();
        let mut old = VersionVector::new();
        old.increment(a);
        let mut new = old.clone();
        new.increment(a);
        let (left, right) = trees(file(b"old", old, 1), file(b"new", new, 2));
        let plan = reconcile(&left, &right, ConflictPolicy::ConflictCopy, 10);
        assert_eq!(plan.pull.len(), 1);
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn concurrent_edits_create_a_conflict_copy() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let mut left_version = VersionVector::new();
        left_version.increment(a);
        let mut right_version = VersionVector::new();
        right_version.increment(b);
        let (left, right) = trees(
            file(b"local", left_version, 2),
            file(b"remote", right_version, 1),
        );
        let plan = reconcile(&left, &right, ConflictPolicy::ConflictCopy, 10);
        assert_eq!(plan.conflicts.len(), 1);
        assert!(plan.conflicts[0].conflict_path.contains("boxdrop-conflict"));
        assert_eq!(plan.push.len(), 1); // local has the newer mtime
        assert_eq!(plan.pull.len(), 1); // the remote loser, at the conflict path
        assert!(plan.pull[0].path.contains("boxdrop-conflict"));
    }

    #[test]
    fn last_write_wins_keeps_only_the_winner() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let mut left_version = VersionVector::new();
        left_version.increment(a);
        let mut right_version = VersionVector::new();
        right_version.increment(b);
        let (left, right) = trees(
            file(b"local", left_version, 2),
            file(b"remote", right_version, 1),
        );
        let plan = reconcile(&left, &right, ConflictPolicy::LastWriteWins, 10);
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.push.len(), 1);
    }

    #[test]
    fn edit_beats_concurrent_deletion() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let mut deleted_version = VersionVector::new();
        deleted_version.increment(a);
        let mut live_version = VersionVector::new();
        live_version.increment(b);
        let (left, right) = trees(
            Entry::tombstone(FileId::new(), deleted_version),
            file(b"still here", live_version, 1),
        );
        let plan = reconcile(&left, &right, ConflictPolicy::ConflictCopy, 10);
        assert_eq!(plan.pull.len(), 1);
        assert!(plan.push.is_empty());
    }
}
