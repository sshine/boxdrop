{
  lib,
  stdenv,
  glib,
}:
stdenv.mkDerivation {
  pname = "gnome-shell-extension-boxdrop";
  version = "0.1.0";

  src = ../gnome/boxdrop;

  nativeBuildInputs = [ glib ];
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    target="$out/share/gnome-shell/extensions/boxdrop@simonshine.dk"
    mkdir -p "$target"
    cp -r . "$target"
    if [ -d "$target/schemas" ]; then
      glib-compile-schemas --strict "$target/schemas"
    fi
    runHook postInstall
  '';

  meta = {
    description = "GNOME Shell top-bar indicator for Boxdrop";
    homepage = "https://github.com/sshine/boxdrop";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
  };

  passthru.extensionUuid = "boxdrop@simonshine.dk";
}
