{ inputs, ... }:
{
  imports = [ inputs.devshell.flakeModule ];

  perSystem =
    { config, pkgs, ... }:
    let
      toolchain = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;
    in
    {
      devshells.default = {
        packages = [
          toolchain
          config.treefmt.build.wrapper
          pkgs.git
          pkgs.jq
          pkgs.just
          pkgs.sqlite
          pkgs.util-linux
          pkgs.iproute2
          pkgs.glib
          pkgs.gjs
          pkgs.gnome-shell
          pkgs.zfs
          pkgs.btrfs-progs
        ];

        env = [
          {
            name = "RUST_BACKTRACE";
            value = "1";
          }
        ];

        devshell.motd = "";
      };
    };
}
