//! Durable state for Boxdrop.
//!
//! SQLite holds the index, the queue, snapshot metadata and the chunk table.
//! Chunk bytes live on disk in a sharded content-addressed store, because a
//! multi-gigabyte video does not belong in a SQLite BLOB. The database is the
//! source of truth for *which* chunks exist; the blob directory is the payload.

mod blob;
mod schema;

pub use blob::BlobStore;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use boxdrop_core::{
    ContentManifest, DeviceId, Entry, EntryKind, Fingerprint, Hash32, RootId, Snapshot,
    TreeManifest, VersionVector,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::schema::MIGRATIONS;

#[derive(Clone, Debug)]
pub struct StoredFile {
    pub path: String,
    pub entry: Entry,
    pub fingerprint: Fingerprint,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub address: String,
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct QueueItem {
    pub id: i64,
    pub root_id: RootId,
    pub path: String,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    pub change_count: u32,
    pub due_at_ms: i64,
    pub attempts: u32,
}

#[derive(Clone, Debug, Default)]
pub struct QueueDepth {
    pub pending: u64,
    pub leased: u64,
    pub failed: u64,
}

pub struct Store {
    conn: Mutex<Connection>,
    blobs: BlobStore,
    state_dir: PathBuf,
}

impl Store {
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self> {
        let state_dir = state_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("creating state directory {}", state_dir.display()))?;
        let db_path = state_dir.join("boxdrop.db");
        let conn =
            Connection::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA auto_vacuum = INCREMENTAL;",
        )?;
        let store = Self {
            conn: Mutex::new(conn),
            blobs: BlobStore::new(state_dir.join("blobs")),
            state_dir,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            let target = index as u32 + 1;
            if version < target {
                conn.execute_batch(migration)
                    .with_context(|| format!("applying schema migration {target}"))?;
                conn.pragma_update(None, "user_version", target)?;
            }
        }
        Ok(())
    }

    pub fn put_root(&self, root_id: RootId, path: &Path, device_id: DeviceId) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO roots(root_id, path, device_id, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(root_id) DO UPDATE SET path = excluded.path, device_id = excluded.device_id",
            params![
                root_id.to_string(),
                path.to_string_lossy(),
                device_id.to_string(),
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn ensure_device_id(&self) -> Result<DeviceId> {
        if let Some(value) = self.get_meta("device_id")? {
            return value.parse().map_err(|error| anyhow::anyhow!("{error}"));
        }
        let device_id = DeviceId::new();
        self.set_meta("device_id", &device_id.to_string())?;
        Ok(device_id)
    }

    pub fn root_by_path(&self, path: &Path) -> Result<Option<(RootId, DeviceId)>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT root_id, device_id FROM roots WHERE path = ?1",
                [path.to_string_lossy().as_ref()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        match row {
            Some((root_id, device_id)) => Ok(Some((root_id.parse()?, device_id.parse()?))),
            None => Ok(None),
        }
    }

    pub fn list_roots(&self) -> Result<Vec<(RootId, PathBuf, DeviceId)>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT root_id, path, device_id FROM roots ORDER BY path")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut roots = Vec::new();
        for row in rows {
            let (root_id, path, device_id) = row?;
            roots.push((root_id.parse()?, PathBuf::from(path), device_id.parse()?));
        }
        Ok(roots)
    }

    pub fn root(&self, root_id: RootId) -> Result<Option<(PathBuf, DeviceId)>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT path, device_id FROM roots WHERE root_id = ?1",
                [root_id.to_string()],
                |row| {
                    let path: String = row.get(0)?;
                    let device: String = row.get(1)?;
                    Ok((path, device))
                },
            )
            .optional()?;
        match row {
            Some((path, device)) => Ok(Some((
                PathBuf::from(path),
                device.parse().map_err(|error| anyhow::anyhow!("{error}"))?,
            ))),
            None => Ok(None),
        }
    }

    pub fn upsert_file(
        &self,
        root_id: RootId,
        path: &str,
        entry: &Entry,
        fingerprint: &Fingerprint,
    ) -> Result<()> {
        let conn = self.conn();
        let manifest = entry
            .manifest
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        conn.execute(
            "INSERT INTO files(
                root_id, path, file_id, kind, size, mtime_ns, ctime_ns, ino, mode,
                content_id, manifest, version, symlink_target
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(root_id, path) DO UPDATE SET
                file_id = excluded.file_id,
                kind = excluded.kind,
                size = excluded.size,
                mtime_ns = excluded.mtime_ns,
                ctime_ns = excluded.ctime_ns,
                ino = excluded.ino,
                mode = excluded.mode,
                content_id = excluded.content_id,
                manifest = excluded.manifest,
                version = excluded.version,
                symlink_target = excluded.symlink_target",
            params![
                root_id.to_string(),
                path,
                entry.file_id.to_string(),
                entry_kind_to_str(entry.kind),
                entry.size as i64,
                entry.mtime_ns,
                fingerprint.ctime_ns,
                fingerprint.ino as i64,
                entry.mode as i64,
                entry.content_id.map(|hash| hash.to_string()),
                manifest,
                serde_json::to_string(&entry.version)?,
                entry.symlink_target,
            ],
        )?;
        Ok(())
    }

    pub fn remove_file(&self, root_id: RootId, path: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM files WHERE root_id = ?1 AND path = ?2",
            params![root_id.to_string(), path],
        )?;
        Ok(())
    }

    pub fn get_file(&self, root_id: RootId, path: &str) -> Result<Option<StoredFile>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT path, file_id, kind, size, mtime_ns, ctime_ns, ino, mode,
                        content_id, manifest, version, symlink_target
                 FROM files WHERE root_id = ?1 AND path = ?2",
                params![root_id.to_string(), path],
                stored_file_from_row,
            )
            .optional()?;
        row.transpose()
    }

    pub fn list_files(&self, root_id: RootId) -> Result<Vec<StoredFile>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT path, file_id, kind, size, mtime_ns, ctime_ns, ino, mode,
                    content_id, manifest, version, symlink_target
             FROM files WHERE root_id = ?1 ORDER BY path",
        )?;
        let rows = statement.query_map([root_id.to_string()], stored_file_from_row)?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row??);
        }
        Ok(files)
    }

    pub fn file_count(&self, root_id: RootId) -> Result<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM files WHERE root_id = ?1 AND kind != 'deleted'",
            [root_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    pub fn put_chunk(&self, hash: Hash32, bytes: &[u8]) -> Result<()> {
        self.blobs.put(hash, bytes)?;
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO chunks(hash, size, created_at_ms) VALUES (?1, ?2, ?3)",
            params![hash.to_string(), bytes.len() as i64, now_ms()],
        )?;
        Ok(())
    }

    pub fn get_chunk(&self, hash: Hash32) -> Result<Vec<u8>> {
        self.blobs.get(hash)
    }

    pub fn has_chunk(&self, hash: Hash32) -> Result<bool> {
        self.blobs.contains(hash)
    }

    pub fn put_content(&self, manifest: &ContentManifest, blocks: &[Vec<u8>]) -> Result<()> {
        if blocks.len() != manifest.blocks.len() {
            bail!(
                "content has {} blocks, manifest has {}",
                blocks.len(),
                manifest.blocks.len()
            );
        }
        for (hash, block) in manifest.blocks.iter().zip(blocks) {
            if Hash32::of(block) != *hash {
                bail!("block hash mismatch while storing content");
            }
            self.put_chunk(*hash, block)?;
        }
        Ok(())
    }

    pub fn chunk_bytes(&self) -> Result<u64> {
        let conn = self.conn();
        let bytes: i64 =
            conn.query_row("SELECT COALESCE(SUM(size), 0) FROM chunks", [], |row| {
                row.get(0)
            })?;
        Ok(bytes as u64)
    }

    pub fn chunk_count(&self) -> Result<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    pub fn create_snapshot(
        &self,
        tree: &TreeManifest,
        parents: &[Hash32],
        message: Option<&str>,
    ) -> Result<Snapshot> {
        let tree_hash = tree.tree_hash();
        let snapshot = Snapshot {
            id: Snapshot::compute_id(
                parents,
                tree.root_id,
                tree_hash,
                tree.device_id,
                now_ms(),
                message,
            ),
            parents: parents.to_vec(),
            root_id: tree.root_id,
            tree_hash,
            device_id: tree.device_id,
            created_at_ms: now_ms(),
            message: message.map(ToOwned::to_owned),
        };
        let entries = tree
            .entries
            .iter()
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect::<Vec<_>>();
        let blob = bincode::serde::encode_to_vec(&entries, bincode::config::standard())?;
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO snapshots(id, root_id, parents, tree_hash, device_id, created_at_ms, message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                snapshot.id.to_string(),
                snapshot.root_id.to_string(),
                serde_json::to_string(&snapshot.parents)?,
                snapshot.tree_hash.to_string(),
                snapshot.device_id.to_string(),
                snapshot.created_at_ms,
                snapshot.message,
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO trees(tree_hash, entries) VALUES (?1, ?2)",
            params![snapshot.tree_hash.to_string(), blob],
        )?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub fn list_snapshots(&self, root_id: RootId, limit: u32) -> Result<Vec<Snapshot>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, parents, root_id, tree_hash, device_id, created_at_ms, message
             FROM snapshots WHERE root_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![root_id.to_string(), limit], |row| {
            let id: String = row.get(0)?;
            let parents: String = row.get(1)?;
            let root_id: String = row.get(2)?;
            let tree_hash: String = row.get(3)?;
            let device_id: String = row.get(4)?;
            Ok((
                id,
                parents,
                root_id,
                tree_hash,
                device_id,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut snapshots = Vec::new();
        for row in rows {
            let (id, parents, root_id, tree_hash, device_id, created_at_ms, message) = row?;
            snapshots.push(Snapshot {
                id: id.parse()?,
                parents: serde_json::from_str(&parents)?,
                root_id: root_id.parse()?,
                tree_hash: tree_hash.parse()?,
                device_id: device_id.parse()?,
                created_at_ms,
                message,
            });
        }
        Ok(snapshots)
    }

    pub fn snapshot_entries(&self, snapshot_id: Hash32) -> Result<Vec<(String, Entry)>> {
        let conn = self.conn();
        let tree_hash: Option<String> = conn
            .query_row(
                "SELECT tree_hash FROM snapshots WHERE id = ?1",
                [snapshot_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(tree_hash) = tree_hash {
            let blob: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT entries FROM trees WHERE tree_hash = ?1",
                    [tree_hash],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(blob) = blob {
                let (entries, _): (Vec<(String, Entry)>, _) =
                    bincode::serde::decode_from_slice(&blob, bincode::config::standard())?;
                return Ok(entries);
            }
        }
        // Compatibility with snapshots written before migration 2.
        let mut statement = conn.prepare(
            "SELECT path, entry FROM snapshot_entries WHERE snapshot_id = ?1 ORDER BY path",
        )?;
        let rows = statement.query_map([snapshot_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (path, entry) = row?;
            entries.push((path, serde_json::from_str(&entry)?));
        }
        Ok(entries)
    }

    pub fn tree_count(&self) -> Result<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM trees", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Enqueue a change. The quiet period doubles per rapid change of the same
    /// path, capped by `max_quiet_ms`. This is the per-file half of the
    /// adaptive debounce.
    pub fn enqueue_change(
        &self,
        root_id: RootId,
        path: &str,
        now_ms: i64,
        base_quiet_ms: i64,
        max_quiet_ms: i64,
    ) -> Result<QueueItem> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let previous: Option<(i64, i64, i64, u32)> = tx
            .query_row(
                "SELECT first_seen_ms, last_seen_ms, due_at_ms, change_count
                 FROM queue WHERE root_id = ?1 AND path = ?2",
                params![root_id.to_string(), path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let (first_seen, count) = match previous {
            Some((first, _, _, count)) => (first, count.saturating_add(1)),
            None => (now_ms, 1),
        };
        let exponent = (count.saturating_sub(1)).min(5);
        let quiet = base_quiet_ms
            .saturating_mul(1_i64 << exponent)
            .min(max_quiet_ms);
        let due_at = now_ms.saturating_add(quiet);
        tx.execute(
            "INSERT INTO queue(root_id, path, first_seen_ms, last_seen_ms, change_count,
                               due_at_ms, state, attempts, lease_until_ms, last_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', 0, NULL, NULL)
             ON CONFLICT(root_id, path) DO UPDATE SET
                last_seen_ms = excluded.last_seen_ms,
                change_count = excluded.change_count,
                due_at_ms = excluded.due_at_ms,
                state = 'pending',
                attempts = 0,
                lease_until_ms = NULL,
                last_error = NULL",
            params![root_id.to_string(), path, first_seen, now_ms, count, due_at],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM queue WHERE root_id = ?1 AND path = ?2",
            params![root_id.to_string(), path],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(QueueItem {
            id,
            root_id,
            path: path.to_owned(),
            first_seen_ms: first_seen,
            last_seen_ms: now_ms,
            change_count: count,
            due_at_ms: due_at,
            attempts: 0,
        })
    }

    pub fn due_changes(&self, now_ms: i64, limit: u32) -> Result<Vec<QueueItem>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, root_id, path, first_seen_ms, last_seen_ms, change_count,
                    due_at_ms, attempts
             FROM queue
             WHERE state = 'pending' AND due_at_ms <= ?1
             ORDER BY due_at_ms, id LIMIT ?2",
        )?;
        let rows = statement.query_map(params![now_ms, limit], queue_item_from_row)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row??);
        }
        Ok(items)
    }

    /// Claim due changes under a lease. A crashed worker's lease expires and the
    /// item is retried. Applying a change is idempotent, so at-least-once is safe.
    pub fn claim_due(&self, now_ms: i64, limit: u32, lease_ms: i64) -> Result<Vec<QueueItem>> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let items = {
            let mut statement = tx.prepare(
                "SELECT id, root_id, path, first_seen_ms, last_seen_ms, change_count,
                        due_at_ms, attempts
                 FROM queue
                 WHERE state = 'pending' AND due_at_ms <= ?1
                 ORDER BY due_at_ms, id LIMIT ?2",
            )?;
            let rows = statement.query_map(params![now_ms, limit], queue_item_from_row)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row??);
            }
            items
        };
        let lease_until = now_ms + lease_ms;
        for item in &items {
            tx.execute(
                "UPDATE queue SET state = 'leased', lease_until_ms = ?1 WHERE id = ?2",
                params![lease_until, item.id],
            )?;
        }
        tx.commit()?;
        Ok(items)
    }

    pub fn complete_change(&self, id: i64) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM queue WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn fail_change(&self, id: i64, now_ms: i64, error: &str) -> Result<()> {
        let conn = self.conn();
        let attempts: u32 =
            conn.query_row("SELECT attempts FROM queue WHERE id = ?1", [id], |row| {
                row.get(0)
            })?;
        let backoff = (1_000_i64 << (attempts.min(6))).min(60_000);
        conn.execute(
            "UPDATE queue SET state = 'pending', attempts = attempts + 1,
                              due_at_ms = ?1, lease_until_ms = NULL, last_error = ?2
             WHERE id = ?3",
            params![now_ms + backoff, error, id],
        )?;
        Ok(())
    }

    pub fn reset_expired_leases(&self, now_ms: i64) -> Result<usize> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE queue SET state = 'pending', lease_until_ms = NULL
             WHERE state = 'leased' AND lease_until_ms IS NOT NULL AND lease_until_ms < ?1",
            [now_ms],
        )?;
        Ok(changed)
    }

    pub fn queue_depth(&self) -> Result<QueueDepth> {
        let conn = self.conn();
        let mut depth = QueueDepth::default();
        let mut statement = conn.prepare("SELECT state, COUNT(*) FROM queue GROUP BY state")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (state, count) = row?;
            match state.as_str() {
                "pending" => depth.pending = count as u64,
                "leased" => depth.leased = count as u64,
                "failed" => depth.failed = count as u64,
                _ => {}
            }
        }
        Ok(depth)
    }

    pub fn queue_paths(&self, root_id: RootId) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT path FROM queue WHERE root_id = ?1 ORDER BY due_at_ms")?;
        let rows = statement.query_map([root_id.to_string()], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row?);
        }
        Ok(paths)
    }

    pub fn add_peer(&self, root_id: RootId, name: &str, address: &str) -> Result<()> {
        let conn = self.conn();
        let id = format!("{root_id}/{name}");
        conn.execute(
            "INSERT INTO peers(id, root_id, name, address, enabled)
             VALUES (?1, ?2, ?3, ?4, 1)
             ON CONFLICT(id) DO UPDATE SET address = excluded.address, enabled = 1",
            params![id, root_id.to_string(), name, address],
        )?;
        Ok(())
    }

    pub fn remove_peer(&self, root_id: RootId, name: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM peers WHERE root_id = ?1 AND name = ?2",
            params![root_id.to_string(), name],
        )?;
        Ok(())
    }

    pub fn list_peers(&self, root_id: RootId) -> Result<Vec<Peer>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, name, address, enabled FROM peers WHERE root_id = ?1 ORDER BY name",
        )?;
        let rows = statement.query_map([root_id.to_string()], |row| {
            Ok(Peer {
                id: row.get(0)?,
                name: row.get(1)?,
                address: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
            })
        })?;
        let mut peers = Vec::new();
        for row in rows {
            peers.push(row?);
        }
        Ok(peers)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        self.set_meta("paused", if paused { "1" } else { "0" })
    }

    pub fn paused(&self) -> Result<bool> {
        Ok(self.get_meta("paused")?.as_deref() == Some("1"))
    }
}

fn stored_file_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<StoredFile>> {
    let path: String = row.get(0)?;
    let file_id: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let size: i64 = row.get(3)?;
    let mtime_ns: i64 = row.get(4)?;
    let ctime_ns: i64 = row.get(5)?;
    let ino: i64 = row.get(6)?;
    let mode: i64 = row.get(7)?;
    let content_id: Option<String> = row.get(8)?;
    let manifest: Option<String> = row.get(9)?;
    let version: String = row.get(10)?;
    let symlink_target: Option<String> = row.get(11)?;
    let parsed = (|| -> Result<StoredFile> {
        let kind = str_to_entry_kind(&kind)?;
        let entry = Entry {
            file_id: file_id.parse()?,
            kind,
            size: size.max(0) as u64,
            mtime_ns,
            mode: mode as u32,
            content_id: content_id.map(|value| value.parse()).transpose()?,
            manifest: manifest
                .map(|value| serde_json::from_str(&value))
                .transpose()?,
            version: serde_json::from_str::<VersionVector>(&version)?,
            symlink_target,
        };
        Ok(StoredFile {
            path,
            fingerprint: Fingerprint {
                size: entry.size,
                mtime_ns,
                ctime_ns,
                ino: ino.max(0) as u64,
                mode: entry.mode,
                kind: entry.kind,
            },
            entry,
        })
    })();
    Ok(parsed)
}

fn queue_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<QueueItem>> {
    let root_id: String = row.get(1)?;
    let parsed = (|| -> Result<QueueItem> {
        Ok(QueueItem {
            id: row.get(0)?,
            root_id: root_id.parse()?,
            path: row.get(2)?,
            first_seen_ms: row.get(3)?,
            last_seen_ms: row.get(4)?,
            change_count: row.get::<_, i64>(5)?.max(0) as u32,
            due_at_ms: row.get(6)?,
            attempts: row.get::<_, i64>(7)?.max(0) as u32,
        })
    })();
    Ok(parsed)
}

fn entry_kind_to_str(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::File => "file",
        EntryKind::Directory => "directory",
        EntryKind::Symlink => "symlink",
        EntryKind::Deleted => "deleted",
    }
}

fn str_to_entry_kind(value: &str) -> Result<EntryKind> {
    match value {
        "file" => Ok(EntryKind::File),
        "directory" => Ok(EntryKind::Directory),
        "symlink" => Ok(EntryKind::Symlink),
        "deleted" => Ok(EntryKind::Deleted),
        other => bail!("unknown entry kind: {other}"),
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("state")).unwrap();
        (dir, store)
    }

    #[test]
    fn file_round_trip() {
        let (_dir, store) = store();
        let root = RootId::new();
        let device = DeviceId::new();
        store
            .put_root(root, Path::new("/tmp/root"), device)
            .unwrap();
        let entry = Entry {
            file_id: boxdrop_core::FileId::new(),
            kind: EntryKind::File,
            size: 5,
            mtime_ns: 1,
            mode: 0o644,
            content_id: Some(Hash32::of(b"hello")),
            manifest: None,
            version: VersionVector::new(),
            symlink_target: None,
        };
        store
            .upsert_file(root, "a.txt", &entry, &Fingerprint::default())
            .unwrap();
        let found = store.get_file(root, "a.txt").unwrap().unwrap();
        assert_eq!(found.entry.size, 5);
        assert_eq!(found.entry.content_id, Some(Hash32::of(b"hello")));
        assert_eq!(store.file_count(root).unwrap(), 1);
        store.remove_file(root, "a.txt").unwrap();
        assert!(store.get_file(root, "a.txt").unwrap().is_none());
    }

    #[test]
    fn chunks_round_trip_and_dedupe() {
        let (_dir, store) = store();
        let hash = Hash32::of(b"payload");
        store.put_chunk(hash, b"payload").unwrap();
        store.put_chunk(hash, b"payload").unwrap();
        assert_eq!(store.get_chunk(hash).unwrap(), b"payload");
        assert!(store.has_chunk(hash).unwrap());
        assert_eq!(store.chunk_count().unwrap(), 1);
    }

    #[test]
    fn queue_doubles_quiet_period() {
        let (_dir, store) = store();
        let root = RootId::new();
        let first = store.enqueue_change(root, "a", 1_000, 100, 10_000).unwrap();
        assert_eq!(first.due_at_ms, 1_100);
        let second = store.enqueue_change(root, "a", 1_050, 100, 10_000).unwrap();
        assert_eq!(second.due_at_ms, 1_250);
        let third = store.enqueue_change(root, "a", 1_100, 100, 10_000).unwrap();
        assert_eq!(third.due_at_ms, 1_500);
        assert_eq!(store.queue_depth().unwrap().pending, 1);
    }

    #[test]
    fn queue_claim_lease_and_recovery() {
        let (_dir, store) = store();
        let root = RootId::new();
        store.enqueue_change(root, "a", 0, 0, 0).unwrap();
        let claimed = store.claim_due(0, 10, 50).unwrap();
        assert_eq!(claimed.len(), 1);
        assert!(store.claim_due(0, 10, 50).unwrap().is_empty());
        assert_eq!(store.reset_expired_leases(100).unwrap(), 1);
        assert_eq!(store.claim_due(100, 10, 50).unwrap().len(), 1);
    }

    #[test]
    fn queue_failure_backs_off() {
        let (_dir, store) = store();
        let root = RootId::new();
        let item = store.enqueue_change(root, "a", 0, 0, 0).unwrap();
        store.claim_due(0, 1, 50).unwrap();
        store.fail_change(item.id, 0, "boom").unwrap();
        assert!(store.due_changes(999, 1).unwrap().is_empty());
        assert_eq!(store.due_changes(1_000, 1).unwrap().len(), 1);
    }

    #[test]
    fn snapshots_keep_entries() {
        let (_dir, store) = store();
        let root = RootId::new();
        let device = DeviceId::new();
        let mut tree = TreeManifest::new(root, device);
        tree.insert(
            "a.txt",
            Entry {
                file_id: boxdrop_core::FileId::new(),
                kind: EntryKind::File,
                size: 5,
                mtime_ns: 1,
                mode: 0o644,
                content_id: Some(Hash32::of(b"hello")),
                manifest: None,
                version: VersionVector::new(),
                symlink_target: None,
            },
        );
        let snapshot = store.create_snapshot(&tree, &[], Some("first")).unwrap();
        let second = store
            .create_snapshot(&tree, &[snapshot.id], Some("second"))
            .unwrap();
        let entries = store.snapshot_entries(snapshot.id).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(store.snapshot_entries(second.id).unwrap().len(), 1);
        assert_eq!(store.list_snapshots(root, 10).unwrap().len(), 2);
        // The second snapshot shares the first snapshot's tree blob.
        assert_eq!(store.tree_count().unwrap(), 1);
    }

    #[test]
    fn peers_round_trip_and_remove() {
        let (_dir, store) = store();
        let root = RootId::new();
        store.add_peer(root, "laptop", "laptop:7777").unwrap();
        store.add_peer(root, "server", "server:7777").unwrap();
        let peers = store.list_peers(root).unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].name, "laptop");
        store.remove_peer(root, "laptop").unwrap();
        assert_eq!(store.list_peers(root).unwrap().len(), 1);
    }

    #[test]
    fn pause_flag_persists() {
        let (_dir, store) = store();
        assert!(!store.paused().unwrap());
        store.set_paused(true).unwrap();
        assert!(store.paused().unwrap());
    }
}
