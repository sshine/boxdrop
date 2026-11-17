# Native filesystem integration

Boxdrop works on any filesystem. ZFS, Btrfs and reflink support are
accelerators, not requirements.

```sh
boxdrop native status
boxdrop native status --json
boxdrop doctor
```

The detector reads `/proc/self/mountinfo`, finds the longest mount point that
covers the root, and reports:

- `zfs` with the dataset name;
- `btrfs` with the subvolume path from `btrfs subvolume show`;
- `reflink` when `FICLONE` succeeds in a probe directory;
- `portable` otherwise.

## Snapshots

```sh
boxdrop native snapshot before-import
boxdrop native changed before-import after-import
```

On ZFS the snapshot is mounted in the dataset's namespace and `changed` uses
`zfs diff -H`. On Btrfs the snapshot is a read-only subvolume under
`.boxdrop-snapshots/` beside the root, and `changed` uses
`btrfs subvolume find-new` with the source snapshot's generation.

Both are local history accelerators. They are **not** the sync protocol:
`zfs send`/`btrfs send` are one-way and depend on snapshot lineage, while
Boxdrop's reconciliation is bidirectional. The content-addressed snapshot DAG
remains the portable history.

## Reflinks

`FICLONE` is probed, not assumed. It is used by `boxdrop-fs` when copying into
history on Btrfs, XFS with `reflink=1`, and OpenZFS 2.2+ with block cloning. It
fails harmlessly on ext4 and the caller falls back to a byte copy.
