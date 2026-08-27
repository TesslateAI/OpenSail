# Kata runtime-rs Firecracker runtime handlers for the VOIE development fabric.
#
# Scope: packet FAB1 boots the fixed jailed Firecracker guest through Kata's
# *runtime-rs* Firecracker path and executes `printf ok` via voie-runner.
# The estate already carries Kata's **Go** Firecracker runtime (`kata-fc`);
# runtime-rs is a different shim binary and therefore needs its own containerd
# runtime handler.
#
# This is a self-contained module rather than an edit to a host profile: the
# seam it uses is the one k3s itself renders into the generated containerd
# configuration:
#
#   imports = ["/var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.d/*.toml"]
#
# so a runtime handler can be added by dropping a file in, with no template
# override.
#
# The patched runtime-rs shim is registered as `kata-fc-rs-voie`. The unpatched
# comparison handler from the source quarry is intentionally not exposed: FAB1
# owns only the jailed Firecracker path with the per-sandbox identity repair.
#
# Provenance: minimally adapted from ginit64/betterdam@99e4520daf44dbe10af28557103249ce7acd4b04
# (`deploy/modules/kata-runtime-rs.nix`), reuse authorized for FAB1; see
# docs/provenance/SOURCES.toml (`betterdam-firecracker`). Product-owned
# identifiers were renamed to VOIE-owned ones; runtime pins, patch, lockfile,
# output hashes, and security behavior are preserved exactly.
{
  config,
  pkgs,
  lib,
  ...
}:
let
  cfg = config.voie.kataRuntimeRs;

  # The upstream revision the tracked patch is written against. The Firecracker
  # path at this revision is byte-identical to release tag 4.0.0, which is why
  # one release tarball can supply the guest assets for both handlers while this
  # source tree supplies the patched shim.
  kataRev = "a985d5239d4f8636dc16849cff8088ced61c5a0d";

  # The same upstream release the fabric role already pins for the Go runtime.
  # The url/hash pair is repeated rather than shared because `kataFixed` is a
  # `let` binding inside precode's fabric role with no seam to import; a build
  # that fetched a *different* revision than the Go handler would make the two
  # runtimes incomparable, so the version is asserted at build time below.
  kataVersion = "4.0.0";
  kataSrc = pkgs.fetchurl {
    url = "https://github.com/kata-containers/kata-containers/releases/download/${kataVersion}/kata-static-${kataVersion}-amd64.tar.zst";
    sha256 = "sha256-LDud/ro1VYK0Cu5GKxKRbJdAZU0CMPaWrfcZ1nsGOow=";
  };

  # Guest assets, hypervisor and jailer come from the release tarball; only the
  # shim binary differs between the two handlers. Keeping one asset tree means a
  # difference measured between `kata-fc-rs` and `kata-fc-rs-voie` cannot be a
  # guest kernel or rootfs difference.
  kataAssets = pkgs.stdenvNoCC.mkDerivation {
    pname = "kata-runtime-rs-assets";
    version = kataVersion;
    src = kataSrc;
    nativeBuildInputs = [ pkgs.zstd ];
    dontConfigure = true;
    dontBuild = true;
    unpackPhase = ''
      mkdir -p $out
      tar --zstd -xf "$src" -C $out
    '';
    installPhase = ''
      # A silent wrong-version pin is the failure mode this repository has been
      # bitten by repeatedly. The tarball must state its own version.
      test "$(cat "$out/opt/kata/VERSION")" = "${kataVersion}"

      # The runtime-rs shim must be present: the Go shim lives in bin/, the
      # runtime-rs shim in runtime-rs/bin/, and a release without the latter
      # would silently fall back to the Go runtime under a runtime-rs name.
      test -x "$out/opt/kata/runtime-rs/bin/containerd-shim-kata-v2"

      # The runtime-rs shim is statically linked, unlike the cgo-built Go shim
      # which precode patchelfs. Assert that, because a dynamically linked shim
      # would need the same interpreter fixup and would otherwise fail at
      # sandbox creation with a bare ENOENT.
      if ldd "$out/opt/kata/runtime-rs/bin/containerd-shim-kata-v2" 2>&1 | grep -qv "statically linked"; then
        echo "runtime-rs shim is not statically linked; interpreter fixup required" >&2
        exit 1
      fi

      # runtime-rs reads its own configuration tree, not the Go one. Rewrite the
      # release's /opt/kata prefix to this store path so every hypervisor,
      # jailer, kernel and rootfs reference is content-addressed.
      fcConfig="$out/opt/kata/share/defaults/kata-containers/runtime-rs/configuration-rs-fc.toml"
      test -f "$fcConfig"
      sed -i "s|/opt/kata|$out/opt/kata|g" "$fcConfig"

      # Diagnostics on: FAB1 qualification requires the first blocker to be
      # reproduced from captured runtime output rather than inferred. Without
      # these the shim reports only a connect timeout.
      sed -i -e 's|^enable_debug = false|enable_debug = true|' "$fcConfig"

      # Runtime-rs's Firecracker repair allocates one non-root uid/gid pair per
      # sandbox from a configured range. The lower bound is deliberately outside
      # the normal system-account space; the allocator reserves each selected
      # pair with an O_EXCL root-owned file and refuses collisions/exhaustion.
      # Only the minimum is pinned here: upstream's module text carries a
      # placeholder where its maximum was redacted before publication, and the
      # accepted repair itself defines the fallback maximum (4_000_000_000) that
      # then applies, so no invented number enters this tree.
      sed -i -e '/^jailer_path =/a jailer_uid_min = 100000' "$fcConfig"

      # Current runtime-rs rejects the release config's historical
      # reconnect_timeout_ms=3000 / dial_timeout_ms=45000 combination. Keep
      # retries frequent enough for a slow first devmapper boot while retaining
      # an explicit bounded overall agent connection budget.
      sed -i -e 's|^dial_timeout_ms = 45000|dial_timeout_ms = 1000|' "$fcConfig"
      sed -i -e '/^dial_timeout_ms =/a reconnect_timeout_ms = 60000' "$fcConfig"

      # Rewrites must have happened...
      grep -q "^path = \"$out/opt/kata/bin/firecracker\"" "$fcConfig"
      grep -q "^jailer_path = \"$out/opt/kata/bin/jailer\"" "$fcConfig"
      # ...and nothing unrewritten may survive. Anchored on the assignment: the
      # rewritten paths contain /opt/kata as a substring of the store path, so a
      # bare substring grep would match every rewritten line.
      if grep -qE '^[a-z_]* *= *"/opt/kata' "$fcConfig"; then
        echo "runtime-rs FC config still references unrewritten /opt/kata" >&2
        exit 1
      fi

      "$out/opt/kata/bin/firecracker" --version
      "$out/opt/kata/bin/jailer" --version
    '';
  };

  # A local development VM may reuse a realized, byte-checked Kata output
  # already present in the workstation store. The override is supplied only
  # through the dev host's explicit impure cache input; production profiles
  # continue to use the pinned release derivation above.
  runtimeAssets = if cfg.assetsOverride != null then cfg.assetsOverride else kataAssets;

  rsFcConfig = "${runtimeAssets}/opt/kata/share/defaults/kata-containers/runtime-rs/configuration-rs-fc.toml";

  # The patched shim, built from pinned upstream source with the tracked patch
  # applied. This is what makes `kata-fc-rs-voie` a deployment artifact rather than
  # something an operator built by hand: the handler cannot be advertised without
  # this derivation having produced the exact bytes that implement the repair.
  kataForkSrc = pkgs.fetchFromGitHub {
    owner = "kata-containers";
    repo = "kata-containers";
    rev = kataRev;
    hash = "sha256-62TWjQURK94uYSyz8dHPENAdV3my6TgHiPA1Uqls0Jk=";
  };

  triple = "x86_64-unknown-linux-gnu";

  patchedShim = pkgs.rustPlatform.buildRustPackage {
    pname = "voie-kata-runtime-rs-shim";
    version = "${kataVersion}-voie";
    src = kataForkSrc;

    # The repair itself. Kept as a plain patch rather than a vendored source tree
    # so a reviewer can read exactly what changed relative to upstream,
    # and so the same file can be offered upstream unmodified.
    patches = [ ../patches/kata-runtime-rs-firecracker-jailer-identity.patch ];

    # Nix needs the dependency set as an evaluation-time path, so the lockfile is
    # tracked in-tree instead of read out of `src` (which would require importing
    # a derivation during evaluation). It is byte-identical to the lockfile the
    # patch above produces, and the cargo setup hook fails the build if the two
    # ever diverge, so the duplication cannot rot silently.
    cargoLock = {
      lockFile = ../patches/kata-runtime-rs-Cargo.lock;
      outputHashes = {
        "api_client-0.1.0" = "sha256-RdwQg6/EI+oGkyNXnu5t1q87oTXev25XpIaE+PWDTx4=";
        "devicemapper-0.34.7" = "sha256-P0P7WrjC0LCBHFwkqoG7Wo2l8fZiToyGc5I0q6LidYs=";
        "devicemapper-sys-0.3.3" = "sha256-P0P7WrjC0LCBHFwkqoG7Wo2l8fZiToyGc5I0q6LidYs=";
        "micro_http-0.1.0" = "sha256-XemdzwS25yKWEXJcRX2l6QzD7lrtroMeJNOUEWGR7WQ=";
        "pcilibs-rs-0.1.0" = "sha256-8IxK4fv3D4p9g8b6OI7luw9aoXwtBM/GUbrpcoNYv80=";
        "regorus-0.9.1" = "sha256-+TCq9r8kTNM0URbcDP4D9/lKA6Bni7+KgrGRTJFbQPM=";
        "s390_pv_core-0.11.0" = "sha256-P275gUoF4JtaKvKPvzhCsBuo882kKCYebtNpCDEmTP0=";
      };
    };

    nativeBuildInputs = [
      pkgs.protobuf
      pkgs.clang
      pkgs.pkg-config
      # libz-sys vendors zlib-ng and configures it with cmake. Present only as a
      # build tool: the shim itself is a plain cargo workspace, so cmake's own
      # configure hook must stay out of the way.
      pkgs.cmake
      # Kata enables openssl-sys's vendored feature, so OpenSSL is compiled from
      # source inside this derivation and its Configure script needs perl. Left
      # vendored on purpose: forcing the system library would change the shim's
      # linkage relative to the binary the measurements were taken against.
      pkgs.perl
    ];
    buildInputs = [ pkgs.openssl ];
    dontUseCmakeConfigure = true;

    env = {
      LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
      PROTOC = "${pkgs.protobuf}/bin/protoc";
      # Keep workstation builds bounded even when the system Nix daemon
      # advertises all host cores to Cargo's build hook.
      NIX_BUILD_CORES = "2";
      CARGO_BUILD_JOBS = "2";
    };

    # runtime-rs's own tests want a live hypervisor and a guest image.
    doCheck = false;

    # Built through upstream's Makefile rather than a bare `cargo build`, because
    # the shim needs a generated `config.rs` and upstream selects the hypervisor
    # feature set per architecture. Reimplementing that selection here would risk
    # producing a binary that differs from the one the measurements were taken
    # against for a reason unrelated to the patch.
    buildPhase = ''
      runHook preBuild
      make -C src/runtime-rs runtime BUILD_TYPE=release TRIPLE=${triple}
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall

      mapfile -t shims < <(find . -type f -name containerd-shim-kata-v2 -path '*/release/*')
      if [ "''${#shims[@]}" -ne 1 ]; then
        echo "expected exactly one release shim, found ''${#shims[@]}: ''${shims[*]}" >&2
        exit 1
      fi

      # A build that silently produced an unpatched shim would be the worst
      # possible outcome: the handler would advertise the repair and run the code
      # that reproduced the blocker. Require the repair to be present in the
      # emitted bytes, not merely in the source that was fed to the compiler.
      if ! grep -qa '\.jailer-identities' "''${shims[0]}"; then
        echo "built shim does not contain the jailer identity repair" >&2
        exit 1
      fi

      install -Dm755 "''${shims[0]}" "$out/bin/containerd-shim-kata-v2"
      runHook postInstall
    '';

    meta = {
      description = "Kata runtime-rs containerd shim with VOIE's per-sandbox Firecracker jailer identity repair";
      mainProgram = "containerd-shim-kata-v2";
    };
  };

  runtimeShim = if cfg.patchedShimOverride != null then cfg.patchedShimOverride else cfg.patchedShim;

  # One handler block per shim. `snapshotter = "devmapper"` is required for the
  # same reason the Go handlers need it: Firecracker has no shared-filesystem
  # support, so a container rootfs must arrive as a block device.
  handlerBlock =
    {
      name,
      shim,
      configPath,
    }:
    ''

      [plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.${name}]
        runtime_type = "io.containerd.kata.v2"
        runtime_path = "${shim}"
        privileged_without_host_devices = true
        pod_annotations = ["io.katacontainers.*"]
        snapshotter = "devmapper"

      [plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.${name}.options]
        ConfigPath = "${configPath}"
    '';

  # Handler name -> shim that will execute it. This is also the list live
  # verification consumes: the actual pod sandbox must be shown to have run the
  # real CRI runtime handler, separately from the RuntimeClass metadata, so the
  # host profile exposes what containerd will actually register here.
  registeredHandlers = {
    kata-fc-rs-voie = "${runtimeShim}/bin/containerd-shim-kata-v2";
  };

  dropIn = pkgs.writeText "voie-kata-fc-rs.toml" (
    ''
      # Generated by nix/runtime/kata-runtime-rs.nix. Imported by the k3s
      # containerd configuration through its rendered `imports` glob.
      version = 3
    ''
    + lib.concatStrings (
      lib.mapAttrsToList (
        name: shim:
        handlerBlock {
          inherit name shim;
          configPath = rsFcConfig;
        }
      ) registeredHandlers
    )
  );

  # The RuntimeClass is deployment state, not something a run
  # creates by hand. k3s applies and reconciles everything in its server
  # manifests directory, so the object is declared once here and converges with
  # the rest of the profile.
  #
  # The RuntimeClass NAME is deliberately not the CRI handler name. A
  # RuntimeClass may declare any `.handler`, so a product that checked only the
  # name would learn nothing about which runtime will execute the workload.
  # Keeping the two strings different is what makes FAB1's separate handler
  # check able to fail.
  runtimeClassName = "voie-firecracker";
  runtimeClassHandler = "kata-fc-rs-voie";

  runtimeClassManifest = pkgs.writeText "voie-runtimeclass-firecracker.yaml" ''
    # Generated by nix/runtime/kata-runtime-rs.nix.
    apiVersion: node.k8s.io/v1
    kind: RuntimeClass
    metadata:
      name: ${runtimeClassName}
      labels:
        io.voie/managed: "true"
    handler: ${runtimeClassHandler}
  '';
in
{
  options.voie.kataRuntimeRs = {
    enable = lib.mkEnableOption ''
      Kata runtime-rs Firecracker runtime handlers (packet FAB1).
      Registers the patched runtime-rs shim as CRI handler `kata-fc-rs-voie` and
      declares the `${runtimeClassName}` RuntimeClass that selects it
    '';

    patchedShim = lib.mkOption {
      type = lib.types.package;
      default = patchedShim;
      description = ''
        Package providing bin/containerd-shim-kata-v2 built from pinned upstream
        Kata source with the tracked jailer-identity repair applied. Registered
        as handler `kata-fc-rs-voie`, the only Firecracker runtime handler in
        this FAB1 slice.

        This defaults to a store-addressed build from the tracked patch. It is an
        option only so an investigation can substitute a differently-patched shim
        deliberately; there is no null case, because a profile that enables this
        module advertises the handler and must therefore install it.
      '';
    };

    # These two options are deliberately paths rather than packages: a local
    # Nix store output can be selected without rebuilding the pinned source
    # derivation or recording a machine-specific store hash in the repository.
    assetsOverride = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Optional realized Kata asset tree for an offline development VM.
        Production profiles leave this null and use the pinned release.
      '';
    };

    patchedShimOverride = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Optional realized VOIE-patched runtime-rs shim for an offline
        development VM. Production profiles leave this null and build the
        tracked patch.
      '';
    };

    criHandlers = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      readOnly = true;
      default = lib.attrNames registeredHandlers;
      description = ''
        CRI handler names this host registers with containerd. Consumed by live
        verification so the advertised RuntimeClass can be checked against the
        runtime handler a sandbox actually ran, separately from its metadata.
      '';
    };

    runtimeClass = lib.mkOption {
      type = lib.types.str;
      readOnly = true;
      default = runtimeClassName;
      description = ''
        Name of the RuntimeClass object this module declares. Deliberately
        different from the CRI handler it selects.
      '';
    };

    assets = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = kataAssets;
      description = "The pinned Kata release tree these handlers run from.";
    };
  };

  config = lib.mkIf cfg.enable {
    # `d` is repeated from the fabric role deliberately: tmpfiles rules are
    # idempotent and this module must not depend on another module having
    # created its parent directory.
    systemd.tmpfiles.rules = [
      "d /var/lib/rancher/k3s/agent/etc/containerd 0755 root root -"
      "d /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.d 0755 root root -"
      "L+ /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.d/voie-kata-fc-rs.toml - - - - ${dropIn}"
      "d /var/lib/rancher/k3s/server/manifests 0700 root root -"
      "L+ /var/lib/rancher/k3s/server/manifests/voie-runtimeclass-firecracker.yaml - - - - ${runtimeClassManifest}"
    ];

    # containerd reads its configuration only at startup.
    systemd.services.k3s.restartTriggers = [
      dropIn
      runtimeAssets
      runtimeShim
    ];

    # The shim-delete path restores a sandbox through
    # RuntimeHandlerManager::cleanup, which calls load_config with no containerd
    # options and no KATA_CONF_FILE (delete is a bare shim invocation), so it
    # probes the default path list — whose first entry is this one. Without a
    # file there, cleanup fails before the sandbox can be restored and the
    # jail torn down. The symlink keeps the store config the single source of
    # truth.
    environment.etc."kata-containers/runtime-rs/configuration.toml".source = rsFcConfig;

    # The shim locates firecracker and the jailer by absolute path from its
    # config, so PATH is not load-bearing for those. It is still extended so an
    # operator on this host can run the exact pinned binaries the runtime uses.
    environment.systemPackages = [ runtimeAssets ];
  };
}
