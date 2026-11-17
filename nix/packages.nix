{ ... }:
{
  perSystem =
    { pkgs, lib, ... }:
    let
      boxdrop = pkgs.callPackage ./_package.nix { };
    in
    {
      packages.default = boxdrop;
      packages.boxdrop = boxdrop;
      packages.boxdrop-daemon = boxdrop;

      checks.default = boxdrop;

      apps.default = {
        type = "app";
        program = lib.getExe' boxdrop "boxdrop";
      };
      apps.boxdropd = {
        type = "app";
        program = lib.getExe' boxdrop "boxdropd";
      };
    };
}
