# Performance and metadata budget

The harness is the measurement tool, not a synthetic microbenchmark:

```sh
boxdrop-harness soak --duration-secs 20 --rate 50 \
  --high-water-mib 64 --low-water-mib 32
```

A 20-second run on the development machine (ext4, two hosts, 50 files/s,
mostly small files with occasional 2 MiB files) produced:

| Metric | Value |
|---|---|
| paths verified per host | 665 |
| logical bytes per host | ~70 MB |
| chunk bytes per host | ~70 MB |
| `boxdrop.db` per host | ~1.5 MB |
| snapshots per host | 5 |
| metadata across both hosts incl. WAL | ~8.5 MB |

That is roughly 1 KB of database per path for this shape, dominated by the
current-file row, the version vector and the shared snapshot blob. The pruner
keeps the corpus bounded; snapshot objects are deduplicated by tree hash, so a
periodic rescan of an idle tree does not grow history.

## Invariants worth watching

- A periodic scan of an unchanged tree must not create a snapshot.
- A batch of N paths must produce at most one snapshot and one peer session.
- A hot path must not produce one transfer per write; the queue's quiet period
  doubles per rapid change, capped by `max_quiet_ms`.
- A session interrupted after some blocks must transfer only the remaining
  blocks next time.
- Pruning files and running `boxdrop gc` must let the database return toward
  its baseline; the harness reports database and blob bytes at the end of each
  run.

## Scale notes

The snapshot tree is a single bincode blob keyed by tree hash, not one SQLite
row per path per revision. Content manifests are shared by content hash and
chunks are shared by BLAKE3. For very large trees the next step is a Merkle tree
of directory manifests, so an unchanged subtree is shared even when a sibling
changes.
