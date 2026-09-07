{ repo }:
final: prev:
let
  rustSrc = final.callPackage ./rust-src.nix { };
  rustPkgs = final.rustPlatform.buildRustPackage {
    pname = "voie";
    version = "0.0.0";
    src = rustSrc;
    cargoLock.lockFile = repo + "/Cargo.lock";
    doCheck = false;
    nativeBuildInputs = [ final.pkg-config ];
    buildInputs = [ final.zstd ];
  };

  kataConfig = final.writeText "voie-kata-firecracker.toml" ''
    [hypervisor.firecracker]
    path = "${final.firecracker}/bin/firecracker"
    jailer_path = "${final.firecracker}/bin/jailer"
    kernel_path = "${final.linuxPackages.kernel}/bzImage"
    jailer_root = "/run/kata-containers/shared/firecracker"
    block_device_driver = "virtio-blk"
    disable_block_device_use = false
    enable_debug = false

    [agent.kata]
    enable_debug = false
  '';
in
{
  voie-cloud = rustPkgs;
  voie-fabricd = rustPkgs;
  voie-runner = rustPkgs;
  voie-pack = rustPkgs;
  voie-app-init = rustPkgs;
  voie-egress = rustPkgs;
  voie-kata-firecracker-config = kataConfig;
  chrome-headless-shell = final.callPackage ./chrome-headless-shell.nix { };

  # Prebuilt immutable activation child entry. Deployment consumes it
  # through VOIE_ACTIVATION_ENTRY; nothing installs or builds at runtime.
  voie-activation-dist = final.callPackage ./activation-dist.nix { };

  # FD-only privileged handoff pair that runs activation children under the
  # dedicated voie-activation account (see nix/modules/control.nix).
  voie-activation-handoff = final.callPackage ./activation-handoff/default.nix { };

  # Production shell: Vite dist, not the source index.html that points at /src/main.ts.
  voie-web =
    let
      src = final.lib.cleanSourceWith {
        src = repo + "/web";
        filter =
          path: type:
          let
            base = baseNameOf path;
          in
          base != "node_modules" && base != "dist";
      };
    in
    final.stdenv.mkDerivation {
      pname = "voie-web";
      version = "0.0.0";
      inherit src;
      pnpmDeps = final.fetchPnpmDeps {
        inherit src;
        pname = "voie-web";
        version = "0.0.0";
        pnpm = final.pnpm;
        fetcherVersion = 3;
        pnpmInstallFlags = [ "--config.minimumReleaseAge=0" ];
        hash = "sha256-IfG9/K8pYkmxS6JRVTgOx0tSFrGgpqW+d9yRGS+fxkY=";
      };
      nativeBuildInputs = [
        final.nodejs_22
        final.pnpm
        final.pnpmConfigHook
      ];
      buildPhase = ''
        runHook preBuild
        pnpm run build
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        mkdir -p "$out/share/voie-web"
        cp -R dist/. "$out/share/voie-web/"
        runHook postInstall
      '';
    };

  voie-guest-rootfs =
    final.runCommand "voie-guest-rootfs"
      {
        nativeBuildInputs = [ final.squashfsTools ];
      }
      ''
        mkdir -p root/usr/bin root/bin
        cp ${rustPkgs}/bin/voie-runner root/usr/bin/voie-runner
        ln -s usr/bin/voie-runner root/bin/voie-runner
        mkdir -p "$out"
        mksquashfs root "$out/rootfs.squashfs" -comp xz -noappend -all-root
      '';

  containerd-shim-voie-firecracker-v2 = final.writeShellScriptBin "containerd-shim-voie-firecracker-v2" ''
    export KATA_CONF_FILE=${kataConfig}
    exec ${final.kata-runtime}/bin/containerd-shim-kata-v2 "$@"
  '';
}
