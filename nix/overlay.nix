{ ... }:
{
  flake.overlays.default = final: _prev: {
    boxdrop = final.callPackage ./_package.nix { };
  };
}
