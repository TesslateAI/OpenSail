# Pinned sandbox infrastructure (pause) image, built entirely from Nix.
#
# voie-devmapper-pause needs `rancher/mirrored-pause:3.6` inside containerd
# BEFORE it can seed the devmapper snapshot; nothing in the estate may depend
# on a registry pull for it. This derivation produces the exact same image
# reference offline: one static /pause binary at the rootfs root (the unit
# and the CRI both exec /pause), tagged with the upstream name.
#
# Same pattern as nix/runtime/voie-runner-image.nix.
{
  dockerTools,
  pkgsStatic,
}:
let
  pause = pkgsStatic.stdenv.mkDerivation {
    pname = "voie-sandbox-pause";
    version = "3.6";

    src = ./.;

    dontConfigure = true;

    buildPhase = ''
      runHook preBuild
      $CC -O2 -static -s -Wall -Werror -o pause ./voie-pause.c
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall
      mkdir -p "$out/bin"
      install -m 0555 pause "$out/bin/pause"
      runHook postInstall
    '';

    meta = {
      description = "Static PID-1 pause binary (signal-safe zombie reaper) for VOIE sandboxes";
    };
  };

  image = dockerTools.buildLayeredImage {
    name = "docker.io/rancher/mirrored-pause";
    tag = "3.6";

    # /pause must be a REAL regular file inside the layer, not a store symlink:
    # `contents` builds the rootfs by symlinking into /nix/store, which dangles
    # on baremetal guests without a Nix store. extraCommands runs in the layer
    # staging root, so this cp lands a plain executable at /pause exactly where
    # the CRI and the seeded devmapper chain expect it.
    extraCommands = ''
      cp "${pause}/bin/pause" ./pause
      chmod 0555 ./pause
    '';

    config = {
      Cmd = [ "/pause" ];
    };

    meta = {
      description = "Offline pinned rancher/mirrored-pause:3.6 replacement for VOIE Firecracker sandboxes";
    };
  };
in
# Expose the static binary for voie-devmapper-pause. Wrapping the image
# keeps the tar store path unchanged so a seeder-only change does not
# force a containerd re-import. Do not put `pause` inside
# buildLayeredImage — that attrset is not passthru.
image
// {
  pause = pause;
  passthru = (image.passthru or { }) // {
    pause = pause;
  };
}
