# Boxdrop

Boxdrop is a two-way file synchronizer that borrows the best parts of four
tools:

- **Dropbox** watches selected directories, queues changes, coalesces hot files,
  and batches unrelated changes.
- **rsync** exchanges a manifest first and transfers only the blocks that are
  missing, over one session.
- **git** keeps content-addressed snapshots and a history DAG, and treats
  concurrent edits as explicit conflicts rather than silent overwrites.
- **ZFS/Btrfs** supply cheap snapshots, change lists and reflinks where the
  filesystem supports them.

The repository is a Cargo workspace with a dendritic Nix flake. The components
are:

| Component | Purpose |
|---|---|
| `boxdrop` | CLI; talks to the daemon when it is running, otherwise one-shot |
| `boxdropd` | background daemon: watcher supervision, durable queue, sync engine, D-Bus |
| `boxdrop-watch` | small watcher process; forwards filesystem events, holds no state |
| `boxdrop-harness` | deterministic simulation and perpetual soak harness |
| GNOME extension | top-bar status, progress, pause and recent-sync menu |

## Status

Milestone 0 (skeleton), milestone 1 (local and TCP sync) and milestone 2
(simulation) are in progress. The daemon and CLI are milestone 3.

## Design shorthand

- A **change** is a path plus a coalesced event count.
- A **batch** is a set of due changes, one manifest exchange and one snapshot.
- A **version vector** decides concurrency; mtime is only a tie-break for the
  conflict winner.
- A **snapshot** is a content-addressed tree, like a git commit.
- A **conflict copy** preserves both sides; `last-write-wins` is opt-in.

## Development

```sh
nix develop
just test
just fmt
just lint
```

## License

MIT OR Apache-2.0. The GNOME extension is GPL-3.0-or-later to satisfy GNOME
Shell extension review expectations.
