{
  stdenv,
  fetchurl,
  unzip,
  makeWrapper,
  patchelf,
  alsa-lib,
  at-spi2-atk,
  atk,
  cairo,
  cups,
  dbus,
  expat,
  glib,
  glibc,
  libgbm,
  libGL,
  libglvnd,
  libdrm,
  libX11,
  libXcomposite,
  libXdamage,
  libXext,
  libXfixes,
  libXrandr,
  libxcb,
  libxkbcommon,
  libxshmfence,
  mesa,
  nspr,
  nss,
  pango,
  systemd,
  lib,
}:
let
  version = "131.0.6778.204";
  libPath = lib.makeLibraryPath [
    alsa-lib
    at-spi2-atk
    atk
    cairo
    cups
    dbus
    expat
    glib
    glibc
    libgbm
    libGL
    libglvnd
    libdrm
    libX11
    libXcomposite
    libXdamage
    libXext
    libXfixes
    libXrandr
    libxcb
    libxkbcommon
    libxshmfence
    mesa
    nspr
    nss
    pango
    systemd
  ];
in
stdenv.mkDerivation {
  pname = "chrome-headless-shell";
  inherit version;
  src = fetchurl {
    url = "https://storage.googleapis.com/chrome-for-testing-public/${version}/linux64/chrome-headless-shell-linux64.zip";
    hash = "sha256-r6rIbjAsSHQkWZGj1QnVKcUOzdRHr//wzcKwhSCQbDI=";
  };
  nativeBuildInputs = [
    unzip
    makeWrapper
    patchelf
  ];
  # Chrome-for-Testing ships its own shared objects; wrap only for NSS/etc.
  dontStrip = true;
  unpackPhase = ''
    runHook preUnpack
    unzip -q $src
    runHook postUnpack
  '';
  installPhase = ''
    runHook preInstall
    mkdir -p $out/share/chrome-headless-shell $out/bin
    cp -a chrome-headless-shell-linux64/. $out/share/chrome-headless-shell/
    interp="$(cat $NIX_CC/nix-support/dynamic-linker)"
    rpath="${libPath}:$out/share/chrome-headless-shell"
    # Chrome-for-Testing is built against a newer glibc than a typical host
    # loader. Point the ELF interpreter at Nix's dynamic linker so LD_LIBRARY_PATH
    # cannot mix host libc with Nix librt.
    find $out/share/chrome-headless-shell -type f -perm -111 | while read -r bin; do
      if patchelf --print-interpreter "$bin" >/dev/null 2>&1; then
        patchelf --set-interpreter "$interp" --set-rpath "$rpath" "$bin"
      fi
    done
    makeWrapper $out/share/chrome-headless-shell/chrome-headless-shell $out/bin/chrome-headless-shell \
      --prefix LD_LIBRARY_PATH : "$rpath"
    runHook postInstall
  '';
  meta = {
    description = "Pinned Chrome-for-Testing headless-shell used by just browser-smoke";
  };
}
