{ ... }:
{
  flake.nixosModules.default =
    {
      config,
      lib,
      pkgs,
      ...
    }:
    let
      cfg = config.services.boxdrop;
      defaultPackage = pkgs.callPackage ./_package.nix { };
      arguments = [
        "--root"
        cfg.root
        "--state"
        "%S/boxdrop"
      ]
      ++ lib.optionals (cfg.peer != null) [
        "--peer"
        cfg.peer
      ]
      ++ lib.optionals (cfg.listen != null) [
        "--listen"
        cfg.listen
      ]
      ++ cfg.extraArgs;
    in
    {
      options.services.boxdrop = {
        enable = lib.mkEnableOption "the Boxdrop synchronization daemon";

        package = lib.mkOption {
          type = lib.types.package;
          default = defaultPackage;
          defaultText = lib.literalExpression "pkgs.callPackage ./_package.nix { }";
          description = "The Boxdrop package providing boxdropd and boxdrop-watch.";
        };

        root = lib.mkOption {
          type = lib.types.str;
          default = "%h/Boxdrop";
          description = "Directory to synchronize. systemd specifiers are allowed.";
        };

        peer = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "Optional peer address, host:port.";
        };

        listen = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "Optional peer listen address, host:port.";
        };

        extraArgs = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = [ ];
          description = "Extra arguments passed to boxdropd.";
        };
      };

      config = lib.mkIf cfg.enable {
        systemd.user.services.boxdrop = {
          Unit = {
            Description = "Boxdrop file synchronization daemon";
            After = [ "graphical-session.target" ];
          };
          Service = {
            ExecStart = "${cfg.package}/bin/boxdropd ${lib.escapeShellArgs arguments}";
            Restart = "on-failure";
            RestartSec = 2;
            StateDirectory = "boxdrop";
          };
          Install.WantedBy = [ "default.target" ];
        };
      };
    };
}
