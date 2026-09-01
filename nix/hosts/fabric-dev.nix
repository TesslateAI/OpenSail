{
  modulesPath,
  lib,
  pkgs,
  ...
}:
let
  # The second QEMU drive is named in the host definition and is consumed
  # inside the guest by the storage unit below. It is never a host path.
  poolDevice = "/dev/disk/by-id/virtio-voie-fabric-pool";
  runnerImage = pkgs.callPackage ../runtime/voie-runner-image.nix { };
  localKataAssets = builtins.getEnv "VOIE_KATA_ASSETS";
  localKataShim = builtins.getEnv "VOIE_KATA_SHIM";
in
{
  imports = [
    "${modulesPath}/virtualisation/qemu-vm.nix"
    ../modules/fabric.nix
  ];

  nixpkgs.hostPlatform = "x86_64-linux";
  system.stateVersion = "26.05";
  networking.hostName = "voie-fabric-dev";

  # The development VM has no estate identity or remote management plane.
  services.openssh.enable = lib.mkForce false;
  services.tailscale.enable = lib.mkForce false;
  networking.firewall.allowedTCPPorts = lib.mkForce [ 7840 ];

  # Keep the VM direct-booted and headless. QEMU's forceAccel check makes a
  # missing or inaccessible host /dev/kvm an explicit failure instead of
  # silently falling back to TCG.
  boot.loader.systemd-boot.enable = lib.mkForce false;
  boot.loader.grub.enable = lib.mkForce false;
  virtualisation.useBootLoader = false;
  virtualisation.graphics = false;
  virtualisation.memorySize = 6144;
  virtualisation.cores = 4;
  virtualisation.diskSize = 12288;
  virtualisation.qemu.forceAccel = true;
  # `-cpu max` is the qemu-vm default. Expose the host virtualization
  # extensions so a nested Firecracker attempt can fail for the real host
  # prerequisite rather than for an artificial CPU model.
  virtualisation.qemu.options = [ "-cpu host" ];
  virtualisation.forwardPorts = [
    {
      from = "host";
      host.address = "127.0.0.1";
      host.port = 17840;
      guest.port = 7840;
    }
  ];
  virtualisation.emptyDiskImages = [
    {
      size = 8192;
      driveConfig.deviceExtraOpts.serial = "voie-fabric-pool";
    }
  ];

  # K3s gets a complete config at first boot. The built-in flannel backend is
  # only for this isolated VM: it avoids a mutable chart download while the
  # RuntimeClass and Kata handler remain the exact production Firecracker path
  # from nix/modules/fabric.nix.
  voie.kataRuntimeRs.assetsOverride =
    if localKataAssets == "" then null else builtins.toPath localKataAssets;
  voie.kataRuntimeRs.patchedShimOverride =
    if localKataShim == "" then null else builtins.toPath localKataShim;

  environment.etc."voie/k3s/config.yaml".text = ''
    cluster-init: true
    write-kubeconfig-mode: "0640"
    node-name: voie-fabric-dev
    disable:
      - traefik
      - servicelb
      - local-storage
    flannel-backend: vxlan
    disable-network-policy: true
    secrets-encryption: true
  '';

  # These values are local declarations, not credentials. Fabricd needs the
  # VG to allocate one block-backed workspace LV per workspace.
  environment.etc."voie/fabric.env".text = ''
    VOIE_FABRIC_NAME=voie-fabric-dev
    VOIE_FABRICD_BIND=0.0.0.0:7840
    VOIE_FABRICD_SQLITE=/var/lib/voie-fabricd/state.sqlite
    VOIE_FABRICD_STAGE_ROOT=/var/lib/voie-fabricd/stage
    VOIE_FABRICD_STAGE_MODE=dev-directory
    VOIE_NODE_NAME=voie-fabric-dev
    VOIE_NAMESPACE=voie-workspace
    VOIE_STORAGE_CLASS=voie-workspace-block
    VOIE_RUNTIME_CLASS=voie-firecracker
    VOIE_RUNNER_IMAGE=voie-runner:c1
    VOIE_WORKSPACE_IMAGE=voie-workspace:v1
    VOIE_JAILER_ROOT=/run/kata-containers/shared/firecracker
    VOIE_WORKSPACE_VG=voie-ws
    VOIE_STORAGE_RUNTIME_POOL=2G
    VOIE_STORAGE_WORKSPACE_POOL=workspace
    VOIE_STORAGE_WORKSPACE_POOL_DATA=2G
    VOIE_STORAGE_WORKSPACE_NORMAL_BUDGET=1G
    VOIE_STORAGE_WORKSPACE_RESTORE_HEADROOM=512M
    VOIE_STORAGE_STAGING=0
    VOIE_STORAGE_WORKSPACE_DEFAULT=256M
    VOIE_STORAGE_WORKSPACE_LARGE=512M
    VOIE_STORAGE_WORKSPACE_ELEVATED=1G
    VOIE_STORAGE_LINEAR_NORMAL_BUDGET=2G
    VOIE_STORAGE_LINEAR_RECOVERY_RESERVE=1G
    VOIE_STORAGE_EMERGENCY_FLOOR=512M
    VOIE_STORAGE_DATABASE_DEV=256M
    VOIE_STORAGE_DATABASE_DEV_ELEVATED=512M
    VOIE_STORAGE_DATABASE_PROD=512M
    VOIE_STORAGE_DATABASE_PROD_ELEVATED=1G
    VOIE_STORAGE_DEPLOYMENT=256M
    VOIE_KUBECTL=k3s kubectl
    VOIE_CRICTL=k3s crictl
    VOIE_KUBECONFIG=/etc/rancher/k3s/k3s.yaml
    VOIE_FABRIC_CERT=/etc/voie/secrets/fabric-server.crt
    VOIE_FABRIC_KEY=/etc/voie/secrets/fabric-server.key
    VOIE_FABRIC_CA=/etc/voie/secrets/fabric-ca.crt
  '';
  environment.etc."voie/images/voie-runner-c1.tar".source = runnerImage;

  systemd.tmpfiles.rules = [
    "d /run/kata-containers/shared/firecracker 0750 root root -"
    "d /run/voie/containerd-devmapper 0700 root root -"
  ];

  # DEV-ONLY: the containerd config template from the shared module with one
  # difference — the devmapper metadata root lives on tmpfs. The dev pool
  # disk is an empty QEMU drive recreated every launch, so ephemeral metadata
  # is the only lifetime that can never record committed chains over a wiped
  # pool. Production keeps its persistent root_path and persistent pool.
  environment.etc."voie/k3s/containerd-config.toml.tmpl".text = lib.mkForce ''
    {{ template "base" . }}

    imports = ["/var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.d/*.toml"]

    [plugins.'io.containerd.snapshotter.v1.devmapper']
      pool_name = "voie--ws-runtime"
      root_path = "/run/voie/containerd-devmapper"
      base_image_size = "10GB"
      async_remove = false
  '';

  # The host launcher (just dev-fabric-up) exposes a read-only virtfs share
  # tagged voie-pki holding this VM's server certificate, server key, and the
  # dev CA. fabricd terminates product-shaped HTTPS+mTLS directly on :7840 —
  # there is no plaintext hop anywhere on the local path. Without the share
  # the mount and therefore fabricd fail closed.
  #
  # This MUST go through virtualisation.fileSystems: qemu-vm.nix replaces the
  # whole fileSystems option with an mkVMOverride of virtualisation.fileSystems,
  # so a direct fileSystems declaration silently vanishes from the generated
  # units — and voie-fabricd Requires=etc-voie-secrets.mount would then point
  # at a unit that does not exist, failing every start.
  virtualisation.fileSystems."/etc/voie/secrets" = {
    device = "voie-pki";
    fsType = "9p";
    options = [
      "trans=virtio"
      "version=9p2000.L"
      "x-systemd.requires=modprobe@9pnet_virtio.service"
      "ro"
      "nodev"
      "nosuid"
    ];
  };

  # The dev VM has no estate identity or rescue channel, so the bare-metal
  # dropbear rescue listener from nix/modules/fabric.nix stays off here.
  systemd.services.dropbear-rescue.enable = lib.mkForce false;

  # The hardened daemon refuses to start without VOIE_FABRIC_CLIENT_SHA256:
  # the SHA-256 fingerprint of the exact control client certificate it will
  # accept. The cert arrives through the read-only voie-pki virtfs (public
  # material; the key stays host-side), so the fingerprint is computed at
  # every boot into a runtime env file — a baked-in store path could never
  # match the launch-time PKI.
  systemd.services.voie-fabric-client-pin = {
    description = "Pin the accepted control client certificate fingerprint for fabricd";
    before = [ "voie-fabricd.service" ];
    after = [ "etc-voie-secrets.mount" ];
    requires = [ "etc-voie-secrets.mount" ];
    wantedBy = [ "multi-user.target" ];
    path = [
      pkgs.coreutils
      pkgs.openssl
      pkgs.gnugrep
    ];
    script = ''
      set -euo pipefail
      test -r /etc/voie/secrets/fabric-client.crt
      # sha256sum prints "<hex>  -" for stdin and this unit's PATH carries no
      # awk provider, so the fingerprint is split with a bash expansion
      # instead of piping through awk (an awk-less PATH failed the unit with
      # exit 127 on every boot).
      raw="$(openssl x509 -in /etc/voie/secrets/fabric-client.crt -outform DER | sha256sum)"
      fp="''${raw%% *}"
      test "''${#fp}" = 64 || {
        echo "implausible client fingerprint '$fp'" >&2
        exit 1
      }
      mkdir -p /run/voie
      printf 'VOIE_FABRIC_CLIENT_SHA256=%s\n' "$fp" > /run/voie/fabric-client.env
      chmod 0644 /run/voie/fabric-client.env
    '';
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      TimeoutStartSec = "30s";
    };
  };

  # One-shot diagnosis: mirror the boot-critical units' output to the VM
  # console so qemu.log carries their actual failure lines. Dev-only — the
  # production profile keeps journal-only. K3S_DEBUG stays off; this changes
  # where output lands, not log verbosity.
  systemd.services.voie-pause-image-load.serviceConfig = {
    StandardOutput = "journal+console";
    StandardError = "journal+console";
  };
  systemd.services.voie-devmapper-pause.serviceConfig = {
    StandardOutput = "journal+console";
    StandardError = "journal+console";
  };

  # The pin gate feeds fabricd's Requires= chain, so its failures must land
  # in qemu.log like the other boot-critical dev units instead of dying
  # journal-only while every later dependency fails invisibly behind it.
  systemd.services.voie-fabric-client-pin.serviceConfig = {
    StandardOutput = "journal+console";
    StandardError = "journal+console";
  };

  # The empty QEMU drive is the only mutable block source. Build the VG and
  # runtime thin pool before either k3s/containerd or fabricd starts. The
  # unit is idempotent so a VM restart never reformats an existing pool.
  systemd.services.voie-dev-storage = {
    description = "Declare the local VOIE workspace and devmapper block pool";
    wantedBy = [ "multi-user.target" ];
    before = [ "k3s.service" ];
    path = [
      pkgs.coreutils
      pkgs.e2fsprogs
      pkgs.lvm2.bin
      pkgs.util-linux
    ];
    script = ''
      set -euo pipefail
      device=${poolDevice}

      for _ in $(seq 1 60); do
        if [ -b "$device" ]; then
          break
        fi
        sleep 1
      done
      test -b "$device"

      if ! pvs --noheadings "$device" >/dev/null 2>&1; then
        pvcreate --yes "$device"
      fi
      if ! vgs --noheadings voie-ws >/dev/null 2>&1; then
        vgcreate voie-ws "$device"
      fi
      # Same names as the production estate (ansible/fabric.yml): the VG is
      # voie-ws, the runtime thin pool is `runtime`, and the Workspace thin
      # pool is `workspace`. Containerd uses only voie--ws-runtime.
      # Leave the rest of the 8G disk unallocated for linear product LVs.
      # Do not create either pool beside a retired product pool; that mixed
      # layout is the live-estate cutover hazard this unit must not invent.
      if lvs --noheadings voie-ws/workspaces >/dev/null 2>&1 \
        || lvs --noheadings voie-ws/ws-root >/dev/null 2>&1; then
        echo "voie-ws still has the retired workspaces thin pool or ws-root; this unit does not wipe it." >&2
        exit 1
      fi
      if ! lvs --noheadings voie-ws/runtime >/dev/null 2>&1; then
        lvcreate --yes --type thin-pool --poolmetadatasize 64M \
          -L 2G -n runtime voie-ws
      fi
      if ! lvs --noheadings voie-ws/workspace >/dev/null 2>&1; then
        lvcreate --yes --type thin-pool --poolmetadatasize 64M \
          -L 2G -n workspace voie-ws
      fi
      lvchange --activate y voie-ws/runtime
      lvchange --activate y voie-ws/workspace
    '';
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      TimeoutStartSec = "90s";
    };
    unitConfig = {
      # This unit feeds var-lib-voie-workspaces.mount, and every mount unit
      # implicitly orders Before=local-fs.target inside the sysinit local-fs
      # chain. As a default-dependency oneshot this service carried
      # After=sysinit.target, closing the cycle
      # journal-catalog-update -> local-fs.target -> mount -> this unit ->
      # sysinit.target; systemd deleted this unit's start job as the cycle
      # breaker and k3s' Requires= died with it, so containerd, both image
      # imports, and fabricd never ran at all. Joining the early-boot window
      # with DefaultDependencies=no and explicit upstream ordering keeps the
      # graph acyclic while still starting strictly after device
      # enumeration and kernel module loading.
      DefaultDependencies = "no";
      After = [
        "systemd-modules-load.service"
        "systemd-udev-trigger.service"
        "systemd-udevd.service"
      ];
      Before = [
        "local-fs.target"
        "shutdown.target"
      ];
      Conflicts = "shutdown.target";
    };
  };

  systemd.services.k3s = {
    after = [
      "network-online.target"
      "voie-dev-storage.service"
    ];
    wants = [ "network-online.target" ];
    requires = [ "voie-dev-storage.service" ];
  };

  # Fabricd creates LVs, formats them, and coordinates block devices with the
  # CRI. The dev VM therefore gives this local-only unit the privilege needed
  # by that existing boundary; the production fabric profile is unchanged.
  systemd.services.voie-fabricd = {
    after = [
      "network-online.target"
      "k3s.service"
      "etc-voie-secrets.mount"
      "voie-fabric-client-pin.service"
    ];
    wants = [ "network-online.target" ];
    requires = [
      "voie-dev-storage.service"
      "etc-voie-secrets.mount"
      "voie-fabric-client-pin.service"
    ];
    path = [
      pkgs.coreutils
      pkgs.e2fsprogs
      pkgs.k3s
      pkgs.lvm2.bin
      pkgs.util-linux
    ];
    serviceConfig = {
      User = lib.mkForce "root";
      Group = lib.mkForce "root";
      EnvironmentFile = lib.mkForce [
        "/etc/voie/fabric.env"
        "/run/voie/fabric-client.env"
      ];
      NoNewPrivileges = lib.mkForce false;
      ProtectSystem = lib.mkForce "full";
      StandardOutput = "journal+console";
      StandardError = "journal+console";
      ReadWritePaths = lib.mkForce [
        "/etc/lvm"
        "/var/lib/voie-fabricd"
        "/run/kata-containers"
      ];
    };
  };

  systemd.services.voie-fabric-image-load.serviceConfig = {
    StandardOutput = "journal+console";
    StandardError = "journal+console";
  };
}
