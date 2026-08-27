# OCI container image for the C1 packet FAB1 demonstration.
#
# The Firecracker guest has no Nix store and no shared filesystem: its only
# input is this container rootfs, delivered as a block device through the
# devmapper snapshotter. Everything the demo touches must therefore be baked
# in here:
#
#   /bin/voie-runner  statically linked against musl, so no interpreter or
#                     shared libraries are required inside the guest;
#   /bin/sh, /bin/printf  busybox applets satisfying the documented runner
#                     contract ("bounded foreground shell");
#   /workspace        the only working directory the runner accepts.
#
# The image carries no credentials, no agent, and nothing else.
{
  dockerTools,
  busybox,
  pkgsStatic,
}:
let
  runner = pkgsStatic.rustPlatform.buildRustPackage {
    pname = "voie-runner";
    version = "0.0.0";
    src = ../..;
    cargoLock.lockFile = ../../Cargo.lock;
    buildAndTestSubdir = "crates/voie-runner";

    # Focused runner behavior lives in the workspace test suite and is run from
    # the devshell; realizing this image must not require executing anything.
    doCheck = false;

    meta = {
      description = "Credentialless VOIE Firecracker guest runner (static build)";
      mainProgram = "voie-runner";
    };
  };
in
dockerTools.buildLayeredImage {
  name = "voie-runner";
  tag = "c1";
  contents = [
    runner
    busybox
  ];

  extraCommands = ''
    mkdir -p workspace
  '';

  config = {
    Entrypoint = [ "/bin/voie-runner" ];
  };

  meta = {
    description = "Guest-rootfs image proving C1: voie-runner executes printf ok inside jailed Firecracker";
  };
}
