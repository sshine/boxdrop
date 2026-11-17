{
  lib,
  rustPlatform,
  makeWrapper,
  pkg-config,
  sqlite,
  ...
}:
let
  workspace = lib.importTOML ../Cargo.toml;
  version = workspace.workspace.package.version;
in
rustPlatform.buildRustPackage {
  pname = "boxdrop";
  inherit version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../README.md
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [ "--workspace" ];
  doCheck = true;

  nativeBuildInputs = [
    makeWrapper
    pkg-config
  ];
  buildInputs = [ sqlite ];

  installPhase = ''
    runHook preInstall
    target_dir="target/''${CARGO_BUILD_TARGET:-x86_64-unknown-linux-gnu}/release"
    if [ ! -x "$target_dir/boxdrop" ]; then target_dir="target/release"; fi
    install -Dm755 "$target_dir/boxdrop" $out/bin/boxdrop
    install -Dm755 "$target_dir/boxdropd" $out/bin/boxdropd
    install -Dm755 "$target_dir/boxdrop-harness" $out/bin/boxdrop-harness
    runHook postInstall
  '';

  postInstall = ''
    wrapProgram $out/bin/boxdrop --prefix PATH : ${lib.makeBinPath [ sqlite ]}
  '';

  meta = {
    description = "Two-way file synchronization with rsync deltas and git-style history";
    mainProgram = "boxdrop";
    license = with lib.licenses; [
      mit
      asl20
    ];
  };
}
