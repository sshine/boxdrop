# Boxdrop GNOME Shell extension

The extension adds a top-bar indicator and a menu for the daemon:

- resting, scanning, syncing, paused, conflict, offline and error states;
- a percentage while a batch is running;
- pause/resume and “Sync now”;
- conflict count, recent batches and traffic totals;
- “Open folder” for the daemon's root.

It talks to `org.boxdrop.Daemon` on the session bus and polls the `StatusJson`
property once a second. If the daemon is not running it shows the offline icon
instead of failing.

## Install from the flake

```sh
nix build .#boxdrop-extension
mkdir -p ~/.local/share/gnome-shell/extensions
cp -r result/share/gnome-shell/extensions/boxdrop@simonshine.dk \
  ~/.local/share/gnome-shell/extensions/
gnome-extensions enable boxdrop@simonshine.dk
```

On NixOS, use the module:

```nix
services.boxdrop = {
  enable = true;
  gnome.enable = true;
  peer = "laptop:7777";
  root = "%h/Boxdrop";
};
```

Log out and back in (or restart GNOME Shell) after enabling the extension.
