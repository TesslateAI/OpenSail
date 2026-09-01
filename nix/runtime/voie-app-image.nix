# Fixed application runtime guest. Immutable /app is mounted at realize time.
{
  dockerTools,
  busybox,
  pkgsStatic,
  nodejs_22,
  python3,
  lib,
}:
let
  python = python3.withPackages (ps: [ ps.psycopg ]);
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
  ];
  extraCommands = ''
    mkdir -p app tmp bin
    ln -sfn ${busybox}/bin/busybox bin/busybox
    ln -sfn busybox bin/wget
    ln -sfn ${python}/bin/python3 bin/python3
    ln -sfn ${init}/bin/voie-app-init bin/voie-app-init
  '';
  config = {
    Entrypoint = [ "/bin/voie-app-init" ];
    WorkingDir = "/app";
  };
  meta = {
    description = "Deployment-owned voie-app:v1 application runtime guest";
  };
}
