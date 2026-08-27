# Privileged-handoff binaries for the activation child boundary.
#
# Two tiny, dependency-free C programs:
#   - voie-activation-launch: execed by voie-cloud in place of Node.js;
#     transfers the bridge socket to the broker and proxies the child exit
#     status back to the parent's wait().
#   - voie-activation-broker: systemd socket-activated executor running as
#     voie-activation; receives one descriptor and one path, execs the
#     pinned Node.js entry with the attested minimal environment.
{
  lib,
  stdenv,
}:
stdenv.mkDerivation {
  pname = "voie-activation-handoff";
  version = "0.0.0";

  src = lib.cleanSource ./.;

  dontConfigure = true;

  buildPhase = ''
    runHook preBuild
    $CC -O2 -Wall -Wextra -Werror \
      -o voie-activation-launch ./activation-launch.c
    $CC -O2 -Wall -Wextra -Werror \
      -o voie-activation-broker ./activation-broker.c
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p "$out/bin"
    install -m 0555 voie-activation-launch "$out/bin/voie-activation-launch"
    install -m 0555 voie-activation-broker "$out/bin/voie-activation-broker"
    runHook postInstall
  '';

  meta = {
    description = "FD-only privileged handoff for voie-cloud activation children (voie-activation boundary)";
  };
}
