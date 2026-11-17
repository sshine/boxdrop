{ ... }:
{
  perSystem =
    { pkgs, ... }:
    let
      extension = pkgs.callPackage ./_extension.nix { };
    in
    {
      packages.boxdrop-extension = extension;
      checks.boxdrop-extension = extension;
    };
}
