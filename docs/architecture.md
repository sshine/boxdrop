# Architecture

## Processes

```
filesystem
   │ inotify
   ▼
boxdrop-watch ── JSON lines over a Unix socket ──► boxdropd
                                                    │
                                    ┌───────────────┼───────────────┐
                                    ▼               ▼               ▼
                              SQLite queue     sync engine      D-Bus
                              (durable)       + snapshot DAG    status
```

`boxdrop-watch` owns the inotify watch and nothing else. It does not open the
database and keeps no authoritative state. It reconnects to the daemon, sends a
`rescan` message, and exits if the daemon disappears; the daemon supervises it
and the periodic rescan covers the gap.

`boxdropd` is the only SQLite writer. It owns the queue, the index, the blob
store, the scheduler, the peer transport and the D-Bus interface.

`boxdrop` is a client. When `org.boxdrop.Daemon` is on the session bus it calls
`SyncNow`, `Pause`, `Resume` and reads `StatusJson`. Otherwise it opens the same
store and performs a one-shot scan/sync.

## Queue semantics

Each queue row is `(root_id, path)`. A change does not create a new row; it
updates `last_seen_ms`, increments `change_count`, and grows the quiet period:

```
quiet = min(max_quiet, base_quiet * 2^(change_count-1))
due_at = last_seen + quiet
```

The scheduler claims a batch of due rows under a lease. A crash leaves a lease
that expires; another worker retries it. Applying a batch is idempotent because
content is addressed by hash and file replacement is atomic. A batch scans all
named paths, does one manifest exchange with the peer, and creates at most one
new snapshot (an unchanged tree does not grow history).

## Conflict rule

A dominating version vector fast-forwards. Only genuinely concurrent edits reach
the conflict policy. The default `conflict-copy` chooses a deterministic winner
by `(mtime_ns, file_id)` so both hosts agree without a coordinator: the winner
keeps the original path, the loser becomes
`<path>.boxdrop-conflict-<device>-<timestamp>`, and both copies are exchanged in
the same session. `last-write-wins` keeps only the winner in the working tree
(the loser remains in snapshot history).

## Metadata

SQLite holds the current index, queue, snapshots and chunk table. Chunk bytes
live in a sharded BLAKE3-addressed blob directory. Content records are shared
between versions; a file edited slightly gets a new manifest but reuses unchanged
chunks. Snapshots are deduplicated by tree hash, so a periodic scan of an idle
tree is free.
