# Fixed development/build guest profile. Applications cannot supply an image name.
#
# The Firecracker guest has no Nix store of its own: dockerTools includes the
# runtime closure in the image. This profile is large by design (D017) so a
# first product can build and test without an image catalog.
{
  dockerTools,
  busybox,
  pkgsStatic,
  rustPlatform,
  pkg-config,
  git,
  nodejs_22,
  python3,
  go,
  rustc,
  cargo,
  gcc,
  gnumake,
  binutils,
  gnutar,
  zstd,
  curl,
  jq,
  cacert,
  coreutils,
  bash,
  gnugrep,
  gnused,
  gawk,
  findutils,
  lib,
}:
let
  helpers = pkgsStatic.rustPlatform.buildRustPackage {
    pname = "voie-guest-helpers";
    version = "0.0.0";
    src = import ../guest-rust-src.nix { inherit lib; };
    cargoLock.lockFile = ../../Cargo.lock;
    buildAndTestSubdir = "crates/voie-runner";
    doCheck = false;
    nativeBuildInputs = [ ];
    meta = {
      description = "Static VOIE guest helpers for the workspace profile";
    };
  };
  pack = rustPlatform.buildRustPackage {
    pname = "voie-pack";
    version = "0.0.0";
    src = import ../guest-rust-src.nix { inherit lib; };
    cargoLock.lockFile = ../../Cargo.lock;
    buildAndTestSubdir = "crates/voie-pack";
    doCheck = false;
    nativeBuildInputs = [ pkg-config ];
    buildInputs = [ zstd ];
    meta = {
      description = "Deterministic Application pack helper for voie-workspace:v1";
    };
  };
in
dockerTools.buildLayeredImage {
  name = "voie-workspace";
  tag = "v1";
  contents = [
    busybox
    helpers
    pack
    git
    nodejs_22
    python3
    go
    rustc
    cargo
    gcc
    gnumake
    binutils
    gnutar
    zstd
    curl
    jq
    cacert
    coreutils
    bash
    gnugrep
    gnused
    gawk
    findutils
  ];
  extraCommands = ''
    mkdir -p workspace tmp bin
    ln -sfn ${python3}/bin/python3 bin/python3
    ln -sfn ${pack}/bin/voie-pack bin/voie-pack
    ln -sfn ${busybox}/bin/busybox bin/busybox
    ln -sfn busybox bin/cat
  '';
  config = {
    Entrypoint = [ "/bin/voie-runner" ];
    Env = [
      "SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt"
      "PATH=/bin:/usr/bin"
    ];
  };
  meta = {
    description = "Deployment-owned voie-workspace:v1 development/build guest";
  };
}
