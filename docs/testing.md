# Testing Boxdrop

## Fast deterministic simulation

```sh
boxdrop-harness sim --seeds 20 --steps 120
```

Each seed creates two temporary roots and two stores, applies seeded random
operations (create, append, overwrite, delete, rename, shared-path edits), syncs
every four operations, and asserts that both trees contain the same paths and
hashes. This is the quick regression test for reconciliation, version vectors
and conflict copies.

## Two-host harness

```sh
boxdrop-harness run --duration-secs 10 --rate 20
```

The parent starts two `boxdrop-harness host` processes. By default each child is
wrapped in `unshare --user --map-root-user --mount --pid --fork`, so it has its
own user, mount and PID namespace. The two hosts communicate over TCP on
loopback; the client performs periodic incremental syncs. Each host writes to
its own directory prefix, so the expected result is an exact match.

At the end the parent performs one final exchange, kills the server and compares
the two trees path-by-path and hash-by-hash. `--no-userns` skips the namespace
wrapper for environments where `unshare` is unavailable.

## Perpetual soak

```sh
boxdrop-harness soak --duration-secs 600 --rate 40 \
  --high-water-mib 512 --low-water-mib 256
```

`soak` adds a pruner to each host. The pruner measures allocated blocks, not
logical size, and deletes oldest files first when the high-water mark is
exceeded. That keeps the workload running indefinitely without filling the
disk, while the metadata report tracks database, WAL and blob growth. The
pattern is adapted from `sftp-relay`'s generator/pruner pair.

## What to look at

The harness prints:

- paths and hashes on both sides after the final sync;
- `db-bytes` and `blob-bytes` per host;
- chunk count, total chunk bytes, file count and snapshot count.

A metadata regression shows up as database or blob growth that does not fall
back after pruning and garbage collection.
