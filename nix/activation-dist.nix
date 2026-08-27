# Immutable prebuilt activation child entry.
#
# Builds the pinned DSH composition in `activation/` inside the sandbox from
# the committed lockfile and installs `dist/` into the store. Deployment and
# tests consume this artifact (or an equivalent prebuilt dist) through
# `VOIE_ACTIVATION_ENTRY`; the activation runtime itself never installs or
# builds anything.
{
  lib,
  stdenv,
  nodejs,
  pnpm,
  fetchPnpmDeps,
}:
stdenv.mkDerivation {
  pname = "voie-activation-dist";
  version = "0.0.0";

  src = lib.cleanSource ../activation;

  nativeBuildInputs = [
    nodejs
    pnpm.configHook
  ];

  pnpmDeps = fetchPnpmDeps {
    pname = "voie-activation-dist";
    version = "0.0.0";
    src = lib.cleanSource ../activation;
    fetcherVersion = 3;
    hash = "sha256-IEoSSsEvdsIm0Q1kNT0twcIoFCdIsEfDY7cmb1s4GmY=";
  };

  buildPhase = ''
    runHook preBuild
    pnpm run build
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p "$out/lib/voie-activation"
    cp -r dist "$out/lib/voie-activation/"
    # The entry resolves bare DSH specifiers through this bundled closure;
    # package.json also pins module semantics (type=module) for dist/*.js.
    cp -a node_modules "$out/lib/voie-activation/"
    cp package.json "$out/lib/voie-activation/"
    runHook postInstall
  '';

  meta = {
    description = "Prebuilt voie-cloud activation child entry (pinned DSH composition)";
  };
}
