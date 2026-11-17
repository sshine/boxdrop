# SSH transport

The SSH transport is the rsync-style deployment path: no server to install, no
port to open, and the existing SSH trust model does authentication and
encryption.

On the remote host, `boxdrop` must be on `PATH` (or pass
`--remote-boxdrop /nix/store/.../bin/boxdrop`). Then:

```sh
boxdrop --state ~/.local/state/boxdrop \
  sync --ssh me@laptop \
  --remote-root /home/me/Boxdrop \
  --remote-state /home/me/.local/state/boxdrop
```

`boxdrop` runs:

```sh
ssh -T me@laptop 'boxdrop' 'serve' '--stdio' '--root' '/home/me/Boxdrop' ...
```

and speaks the same framed protocol the TCP transport uses. One session
exchanges manifests, fetches only missing blocks, pushes local winners and
preserves conflict copies. The remote process exits when the session ends.

Options:

- `--ssh-port` for a non-standard SSH port;
- `--remote-root` and `--remote-state` because the remote paths are almost never
  the local ones;
- `--remote-boxdrop` for a remote binary outside `PATH`;
- `--conflict-policy conflict-copy|last-write-wins|manual`.

The remote command is built from individually shell-quoted arguments, and the
destination is rejected if it starts with `-` or contains whitespace or control
characters.

The daemon can also listen for TCP sessions (`boxdropd --listen 0.0.0.0:7777`)
or a one-shot server can be started with `boxdrop serve --listen ...`. SSH is
the recommended default for machines behind NAT.

Peers can be stored per root so the daemon picks them up on every start:

```sh
boxdrop peer add laptop laptop:7777
boxdrop peer list
boxdrop peer remove laptop
```

`boxdropd --peer host:port` adds one more address for that run; the two sources
are merged and each peer is attempted independently.

## Resume

A session is idempotent. If the connection drops after some blocks have been
written, the next session recomputes the manifest and requests only the blocks
still missing. Content is written to a temporary file and renamed only after
the whole content hash matches, so an interrupted transfer never exposes a
partial file.
