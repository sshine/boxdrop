/// Each string is one migration, applied in order and recorded in
/// `PRAGMA user_version`.
pub const MIGRATIONS: &[&str] = &[r#"
    CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS roots (
        root_id       TEXT PRIMARY KEY,
        path          TEXT NOT NULL,
        device_id     TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS files (
        root_id        TEXT NOT NULL,
        path           TEXT NOT NULL,
        file_id        TEXT NOT NULL,
        kind           TEXT NOT NULL,
        size           INTEGER NOT NULL,
        mtime_ns       INTEGER NOT NULL,
        ctime_ns       INTEGER NOT NULL,
        ino            INTEGER NOT NULL,
        mode           INTEGER NOT NULL,
        content_id     TEXT,
        manifest       TEXT,
        version        TEXT NOT NULL,
        symlink_target TEXT,
        PRIMARY KEY (root_id, path)
    );
    CREATE INDEX IF NOT EXISTS files_by_content ON files(content_id);

    CREATE TABLE IF NOT EXISTS chunks (
        hash          TEXT PRIMARY KEY,
        size          INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS queue (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        root_id        TEXT NOT NULL,
        path           TEXT NOT NULL,
        first_seen_ms  INTEGER NOT NULL,
        last_seen_ms   INTEGER NOT NULL,
        change_count   INTEGER NOT NULL,
        due_at_ms      INTEGER NOT NULL,
        state          TEXT NOT NULL,
        attempts       INTEGER NOT NULL,
        lease_until_ms INTEGER,
        last_error     TEXT,
        UNIQUE (root_id, path)
    );
    CREATE INDEX IF NOT EXISTS queue_due ON queue(state, due_at_ms);

    CREATE TABLE IF NOT EXISTS snapshots (
        id            TEXT PRIMARY KEY,
        root_id       TEXT NOT NULL,
        parents       TEXT NOT NULL,
        tree_hash     TEXT NOT NULL,
        device_id     TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        message       TEXT
    );
    CREATE INDEX IF NOT EXISTS snapshots_by_root ON snapshots(root_id, created_at_ms DESC);

    CREATE TABLE IF NOT EXISTS snapshot_entries (
        snapshot_id TEXT NOT NULL,
        path        TEXT NOT NULL,
        entry       TEXT NOT NULL,
        PRIMARY KEY (snapshot_id, path)
    );

    CREATE TABLE IF NOT EXISTS peers (
        id         TEXT PRIMARY KEY,
        root_id    TEXT NOT NULL,
        name       TEXT NOT NULL,
        address    TEXT NOT NULL,
        enabled    INTEGER NOT NULL DEFAULT 1
    );
    "#];
