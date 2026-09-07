# Fixed application runtime guest. Immutable /app is mounted at realize time.
{
  dockerTools,
  busybox,
  pkgsStatic,
  nodejs_22,
  python3,
  lib,
  stdenv,
}:
let
  python = python3.withPackages (ps: [ ps.psycopg ]);
  bindAny = stdenv.mkDerivation {
    pname = "voie-bind-any";
    version = "0.0.0";
    src = ./voie-bind-any.c;
    dontUnpack = true;
    buildPhase = ''
      $CC -shared -fPIC -O2 -o libvoie-bind-any.so $src -ldl
    '';
    installPhase = ''
      mkdir -p $out/lib
      cp libvoie-bind-any.so $out/lib/
    '';
  };
  init = pkgsStatic.rustPlatform.buildRustPackage {
    pname = "voie-app-init";
    version = "0.0.0";
    src = import ../guest-rust-src.nix { inherit lib; };
    cargoLock.lockFile = ../../Cargo.lock;
    buildAndTestSubdir = "crates/voie-app-init";
    doCheck = false;
    meta = {
      description = "Process supervisor for one Application argv";
      mainProgram = "voie-app-init";
    };
  };
in
dockerTools.buildLayeredImage {
  name = "voie-app";
  tag = "v1";
  contents = [
    busybox
    init
    nodejs_22
    python
    bindAny
  ];
  extraCommands = ''
    mkdir -p app tmp bin
    ln -sfn ${busybox}/bin/busybox bin/busybox
    ln -sfn busybox bin/wget
    ln -sfn ${python}/bin/python3 bin/python3
    ln -sfn ${init}/bin/voie-app-init bin/voie-app-init
    mkdir -p lib
    ln -sfn ${bindAny}/lib/libvoie-bind-any.so lib/libvoie-bind-any.so
  '';
  config = {
    Entrypoint = [ "/bin/voie-app-init" ];
    WorkingDir = "/app";
  };
  meta = {
    description = "Deployment-owned voie-app:v1 application runtime guest";
  };
}
