{
  config,
  lib,
  pkgs,
  ...
}:
let
  # Pinned offline pause image (see nix/runtime/voie-pause-image.nix): the
  # devmapper seeding unit needs rancher/mirrored-pause:3.6 inside containerd
  # before it can prepare the sandbox snapshot, and no estate path may depend
  # on a registry pull for that.
  pauseImage = pkgs.callPackage ../runtime/voie-pause-image.nix { };
  pauseBin = pauseImage.pause;
  runnerImage = pkgs.callPackage ../runtime/voie-runner-image.nix { };
  workspaceImage = pkgs.callPackage ../runtime/voie-workspace-image.nix { };
  appImage = pkgs.callPackage ../runtime/voie-app-image.nix { };
  postgresImage = pkgs.callPackage ../runtime/voie-postgres-image.nix { };
  gatewayImage = pkgs.callPackage ../runtime/voie-gateway-image.nix { };
in
{
  imports = [ ../runtime/kata-runtime-rs.nix ];

  # D005: portable jailed Firecracker through the patched Kata runtime-rs is
  # the only runtime. The runner packet's self-installing module owns the
  # exact shim, assets, containerd drop-in, and RuntimeClass manifest.
  voie.kataRuntimeRs.enable = true;

  users.groups.voie-fabricd = { };

  users.users.voie = {
    isNormalUser = true;
    extraGroups = [ "wheel" ];
  };

  users.users.voie-fabricd = {
    isSystemUser = true;
    group = "voie-fabricd";
    home = "/var/lib/voie-fabricd";
    createHome = true;
  };

  security.sudo.wheelNeedsPassword = false;
  security.sudo.execWheelOnly = true;

  services.openssh.enable = true;
  services.openssh.settings = {
    PasswordAuthentication = false;
    KbdInteractiveAuthentication = false;
    PermitRootLogin = "prohibit-password";
  };

  services.tailscale.enable = true;

  networking.firewall.enable = true;
  # Cilium iptables masquerade delivers pod->world as to-stack. Strict
  # rpfilter in mangle PREROUTING drops those packets when iif is not
  # cilium_host, so CoreDNS and Application egress never leave the node.
  networking.firewall.checkReversePath = "loose";
  networking.firewall.allowedTCPPorts = [
    22
    2222
  ];
  networking.firewall.trustedInterfaces = [
    "tailscale0"
    "cni0"
    "cilium_host"
    "cilium_net"
    "cilium_vxlan"
    "cilium_geneve"
  ];

  boot.kernelModules = [
    "kvm-intel"
    "kvm-amd"
    "vhost_net"
    "vhost_vsock"
    "dm_thin_pool"
    "dm_snapshot"
    "loop"
  ];

  boot.kernelParams = [
    "cgroup_enable=memory"
    # Pull dropbear into the initial boot transaction even if a leftover
    # voie-ws mount hangs local-fs.target. sysinit.target is After=local-fs,
    # so WantedBy=sysinit never starts a listener on a hung boot.
    "systemd.wants=dropbear-rescue.service"
    "systemd.mask=var-lib-voie-workspaces.mount"
  ];

  environment.systemPackages = [
    pkgs.voie-fabricd
    pkgs.voie-runner
    pkgs.python3 # Ansible managed-host interpreter
    pkgs.k3s
    pkgs.kubectl
    pkgs.kubernetes-helm
    pkgs.lvm2
    pkgs.thin-provisioning-tools
    pkgs.e2fsprogs
    pkgs.cryptsetup
    pkgs.util-linux
    pkgs.psmisc
    pkgs.findutils
    pkgs.dropbear
    pkgs.iptables
    pkgs.openssl
    pkgs.curl
    pkgs.jq
    pkgs.tailscale
  ];

  environment.etc."voie/guest-rootfs".source = "${pkgs.voie-guest-rootfs}/rootfs.squashfs";

  systemd.tmpfiles.rules = [
    "d /etc/voie 0750 root root -"
    "d /etc/voie/secrets 0700 root root -"
    "d /etc/voie/k3s 0750 root root -"
    "d /etc/voie/k8s 0750 root root -"
    "d /var/lib/voie-fabricd 0750 voie-fabricd voie-fabricd -"
    "d /var/lib/voie-fabricd/stage 0750 voie-fabricd voie-fabricd -"
    "d /var/lib/rancher/k3s/agent/etc/containerd 0750 root root -"
    "L+ /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl - - - - /etc/voie/k3s/containerd-config.toml.tmpl"
    # Ansible verify and operator shells use /bin/bash. NixOS only ships /bin/sh.
    "L+ /bin/bash - - - - ${pkgs.bashInteractive}/bin/bash"
  ];

  environment.etc."voie/k3s/containerd-config.toml.tmpl".text = ''
    {{ template "base" . }}

    # The patched runtime-rs handler (kata-fc-rs-voie) and its RuntimeClass
    # arrive through the config-v3 drop-in glob owned by
    # nix/runtime/kata-runtime-rs.nix.
    imports = ["/var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.d/*.toml"]

    # Kata Firecracker has no shared-fs rootfs. The handler sets
    # snapshotter = "devmapper"; this plugin must be configured or containerd
    # leaves it in skip and sandbox unpack fails.
    #
    # CRI still unpacks rancher/mirrored-pause onto overlayfs (the default
    # image snapshotter). A first kata sandbox then Prepare()s that chain ID
    # on devmapper as an empty ext4, so the guest execs /pause against
    # lost+found. voie-devmapper-pause copies /pause onto the devmapper
    # snapshot before voie-fabricd starts.
    [plugins.'io.containerd.snapshotter.v1.devmapper']
      pool_name = "voie--ws-runtime"
      root_path = "/var/lib/rancher/k3s/agent/containerd/io.containerd.snapshotter.v1.devmapper"
      base_image_size = "10GB"
      async_remove = false
  '';

  # Dropbear must listen before local-fs. A leftover voie-ws mount or
  # missing ws-root hangs local-fs.target. WantedBy=sysinit/network is too
  # late: those targets are After=local-fs, so the unit is never started
  # on a hung boot. local-fs-pre plus kernel systemd.wants= pulls it into
  # the initial transaction; DefaultDependencies=no keeps it from waiting.
  systemd.services.dropbear-rescue = {
    description = "Dropbear rescue SSH on TCP/2222";
    wantedBy = [ "local-fs-pre.target" ];
    after = [
      "systemd-udevd.service"
      "systemd-modules-load.service"
    ];
    unitConfig = {
      DefaultDependencies = "no";
      Conflicts = "shutdown.target";
      Before = [
        "local-fs.target"
        "shutdown.target"
      ];
    };
    serviceConfig = {
      Type = "simple";
      ExecStartPre = [
        "${pkgs.coreutils}/bin/mkdir -p /var/lib/dropbear /root/.ssh /etc/dropbear"
        "${pkgs.bash}/bin/bash -c 'test -e /var/lib/dropbear/dropbear_ed25519_host_key || ${pkgs.dropbear}/bin/dropbearkey -t ed25519 -f /var/lib/dropbear/dropbear_ed25519_host_key'"
        "-${pkgs.bash}/bin/bash -c '${pkgs.coreutils}/bin/install -m 600 /etc/ssh/authorized_keys.d/root /root/.ssh/authorized_keys; ${pkgs.coreutils}/bin/install -m 600 /etc/ssh/authorized_keys.d/root /etc/dropbear/authorized_keys'"
      ];
      ExecStart = "${pkgs.dropbear}/bin/dropbear -F -E -p 2222 -s -g -r /var/lib/dropbear/dropbear_ed25519_host_key";
      Restart = "always";
      RestartSec = "1s";
    };
  };

  # OS disk is not the Fabric VG. udev auto-activation of leftover product
  # LVs can hang local-fs and drop SSH. Ansible and k3s activate voie-ws
  # explicitly after sshd is already listening.
  environment.etc."lvm/lvm.conf".text = lib.mkAfter ''
    activation/auto_activation_volume_list = [ ]
  '';

  # Systemd stage-1 (pulled in by initrd SSH + contents below) rejects
  # boot.initrd.preLVMCommands. Put the same empty auto-activation list in
  # the initrd lvm.conf so vgchange does not bind leftover product LVs before
  # network.target / sshd.
  boot.initrd.systemd.contents."/etc/lvm/lvm.conf".text = lib.mkAfter ''
    activation/auto_activation_volume_list = [ ]
  '';
  boot.initrd.availableKernelModules = [
    "nvme"
    "ahci"
    "sd_mod"
    "xhci_pci"
    "e1000e"
    "igb"
    "igc"
    "ixgbe"
    "r8169"
  ];

  # If leftover product LVs still hang stage-1, sshd never starts. Initrd
  # SSH on 2222 uses the same root/voie keys the live host already trusts,
  # and stays off when this profile has no keys (flake `fabric` / fabric-dev).
  # Host keys are the running sshd keys copied at activation; root is not an
  # encrypted initrd-unlock disk, so this is rescue, not disk-unlock.
  boot.initrd.network = lib.mkIf (
    (config.users.users.root.openssh.authorizedKeys.keys != [ ])
    || (config.users.users.root.openssh.authorizedKeys.keyFiles != [ ])
    || (config.users.users.voie.openssh.authorizedKeys.keys != [ ])
    || (config.users.users.voie.openssh.authorizedKeys.keyFiles != [ ])
  ) {
    enable = true;
    ssh = {
      enable = true;
      port = 2222;
      authorizedKeys =
        config.users.users.root.openssh.authorizedKeys.keys
        ++ config.users.users.voie.openssh.authorizedKeys.keys;
      authorizedKeyFiles =
        config.users.users.root.openssh.authorizedKeys.keyFiles
        ++ config.users.users.voie.openssh.authorizedKeys.keyFiles;
      hostKeys = [
        "/etc/ssh/ssh_host_ed25519_key"
        "/etc/ssh/ssh_host_rsa_key"
      ];
    };
  };

  systemd.services.sshd = {
    wantedBy = lib.mkForce [ "local-fs-pre.target" ];
    after = lib.mkForce [
      "systemd-udevd.service"
      "systemd-modules-load.service"
    ];
    wants = lib.mkForce [ ];
    unitConfig = {
      DefaultDependencies = "no";
      Conflicts = "shutdown.target";
      Before = [
        "local-fs.target"
        "shutdown.target"
      ];
    };
  };

  # Empty auto_activation_volume_list leaves every voie-ws LV inactive
  # across reboot so leftover product volumes cannot hang stage-1. k3s then
  # starts before /dev/voie-ws/runtime exists. `--noudevsync` skips udev
  # node creation and lvchange fails with `tmeta: open failed`. Activate
  # only infrastructure LVs here, with udev sync, after SSH is already
  # listening. Product LVs stay inactive until voie-fabricd claims them.
  # There is no Fabric staging LV.
  systemd.services.voie-fabric-lvm = {
    description = "Activate Fabric runtime and workspace LVs";
    wantedBy = [ "multi-user.target" ];
    after = [
      "local-fs.target"
      "voie-dev-storage.service"
    ];
    before = [
      "k3s.service"
      "voie-fabricd.service"
    ];
    path = [
      pkgs.coreutils
      pkgs.lvm2.bin
      pkgs.thin-provisioning-tools
      pkgs.util-linux
    ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      TimeoutStartSec = "60s";
      ExecStart = pkgs.writeShellScript "voie-fabric-lvm" ''
        set -euo pipefail
        activate() {
          local spec="$1"
          ${pkgs.coreutils}/bin/timeout -k 5 30 ${pkgs.lvm2.bin}/bin/lvchange --activate y "$spec"
        }
        if ! ${pkgs.lvm2.bin}/bin/vgs voie-ws >/dev/null 2>&1; then
          echo "voie-fabric-lvm: VG voie-ws absent; first install waits for Ansible or voie-dev-storage"
          exit 0
        fi
        if ${pkgs.lvm2.bin}/bin/lvs voie-ws/runtime >/dev/null 2>&1; then
          activate voie-ws/runtime
          test -e /dev/voie-ws/runtime
        fi
        if ${pkgs.lvm2.bin}/bin/lvs voie-ws/workspace >/dev/null 2>&1; then
          activate voie-ws/workspace
        fi
      '';
    };
  };

  systemd.services.k3s = {
    description = "K3s server for the VOIE Firecracker fabric";
    wantedBy = [ "multi-user.target" ];
    after = [
      "network-online.target"
      "voie-fabric-lvm.service"
    ];
    wants = [
      "network-online.target"
      "voie-fabric-lvm.service"
    ];
    path = [
      pkgs.k3s
      pkgs.iptables
      pkgs.util-linux
      pkgs.coreutils
      pkgs.lvm2.bin
      pkgs.e2fsprogs
      pkgs.thin-provisioning-tools
    ];
    serviceConfig = {
      # k3s does not signal systemd readiness in this direct service form.
      # Treat process launch as the ordering boundary; dependent units have
      # their own bounded readiness probes rather than waiting forever for a
      # notification that never arrives.
      Type = "simple";
      KillMode = "process";
      Delegate = "yes";
      LimitNOFILE = "1048576";
      LimitNPROC = "infinity";
      LimitCORE = "infinity";
      TasksMax = "infinity";
      TimeoutStartSec = "90s";
      Restart = "always";
      RestartSec = "5s";
      # Product VG is not udev-activated (see lvm.conf). k3s/containerd
      # only need the runtime thin pool. Activating the whole VG would
      # re-bind leftover product LVs (and a retired `workspaces` pool)
      # before SSH or a DESTROY wipe can run. voie-fabricd activates
      # claimed product LVs after it starts.
      # `--activate y` is manual activation. `--activate ay` honors
      # auto_activation_volume_list and would no-op with the empty list above.
      # Missing runtime is first-install: Ansible creates it, then starts k3s.
      # Do not pass --noudevsync: thin-pool tmeta/tdata nodes come from udev.
      ExecStartPre = pkgs.writeShellScript "voie-runtime-activate" ''
        set -euo pipefail
        if ${pkgs.lvm2.bin}/bin/lvs voie-ws/runtime >/dev/null 2>&1; then
          ${pkgs.coreutils}/bin/timeout -k 5 30 ${pkgs.lvm2.bin}/bin/lvchange --activate y voie-ws/runtime
          test -e /dev/voie-ws/runtime
        fi
      '';
      ExecStart = "${pkgs.k3s}/bin/k3s server --config /etc/voie/k3s/config.yaml";
      DeviceAllow = [
        "/dev/kvm rwm"
        "/dev/kmsg rwm"
        "/dev/vhost-vsock rwm"
        "/dev/vhost-net rwm"
        "/dev/net/tun rwm"
        "/dev/mapper/control rwm"
        "/dev/loop-control rwm"
        "block-device-mapper rw"
        "block-blkext rw"
        "block-loop rw"
      ];
    };
  };

  # Same pattern as the dev VM's runner-image import: the pinned tarball is
  # a Nix store artifact pushed into local containerd at boot — no registry,
  # no host-side manual ctr invocation.
  systemd.services.voie-pause-image-load = {
    description = "Load the pinned VOIE pause image into local containerd";
    wantedBy = [ "multi-user.target" ];
    after = [ "k3s.service" ];
    requires = [ "k3s.service" ];
    before = [ "voie-devmapper-pause.service" ];
    path = [
      pkgs.coreutils
      pkgs.k3s
    ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      TimeoutStartSec = "300s";
      ExecStart = pkgs.writeShellScript "voie-pause-image-load" ''
        set -euo pipefail
        IMAGE=docker.io/rancher/mirrored-pause:3.6
        deadline=$(( $(date +%s) + 240 ))

        # Bounded readiness probe: a wedged containerd must fail the unit,
        # not consume its whole timeout inside one hung ctr call.
        ready=0
        while [ "$(date +%s)" -lt "$deadline" ]; do
          if timeout 10 k3s ctr -n k8s.io images ls >/dev/null 2>&1; then
            ready=1
            break
          fi
          sleep 1
        done
        if [ "$ready" != 1 ]; then
          echo "containerd did not become reachable" >&2
          exit 1
        fi


        # pipefail-safe tag probe: the listing is captured to completion
        # BEFORE matching. `grep -qF` on a live ctr pipe closes the read end
        # at the first match while ctr still writes later rows, killing ctr
        # with SIGPIPE (exit 141); with pipefail that turns a successful
        # match into pipeline failure and the caller loops forever. Here ctr
        # always finishes (or fails) before the fixed-substring match runs.
        tag_present() {
          local listing
          listing="$(k3s ctr -n k8s.io images ls 2>/dev/null)" || return 1
          [[ "$listing" == *"$1"* ]]
        }

        # Import attempts are individually time-boxed; success is proven by
        # the exact tag being present, never by the exit status alone. The
        # per-attempt budget scales with the tar size (>=20 MiB/s sustained
        # unpack floor, 30s minimum) so a real first import completes instead
        # of being killed mid-import every round; the overall deadline still
        # bounds the unit, and each attempt is capped at the time remaining.
        tar_bytes="$(stat -c %s '${pauseImage}')"
        import_secs=$(( tar_bytes / (20 * 1024 * 1024) + 30 ))
        while [ "$(date +%s)" -lt "$deadline" ]; do
          remaining=$(( deadline - $(date +%s) ))
          attempt=$import_secs
          [ "$attempt" -gt "$remaining" ] && attempt=$remaining
          [ "$attempt" -ge 1 ] || break
          if timeout "$attempt" k3s ctr -n k8s.io images import ${pauseImage} >/dev/null 2>&1 &&
            tag_present "$IMAGE"; then
            exit 0
          fi
          sleep 1
        done
        echo "containerd did not accept the pinned pause image $IMAGE within deadline" >&2
        exit 1
      '';
    };
  };

  # DESTROY of the runtime pool leaves containerd snapshot names whose thin
  # devices are gone. Kata Prepare of any guest image then fails with
  # "device metadata not found". Remove those ghosts before pause seeds
  # and fabricd admits a sandbox. A snapshot that still Prepare()s is kept.
  systemd.services.voie-devmapper-gc = {
    description = "Remove leftover containerd devmapper snapshots after a runtime-pool DESTROY";
    wantedBy = [ "multi-user.target" ];
    after = [
      "k3s.service"
      "voie-pause-image-load.service"
    ];
    requires = [ "k3s.service" ];
    before = [
      "voie-devmapper-pause.service"
      "voie-fabricd.service"
    ];
    path = [
      pkgs.k3s
      pkgs.coreutils
      pkgs.gawk
    ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      TimeoutStartSec = "90";
      ExecStart = pkgs.writeShellScript "voie-devmapper-gc" ''
        set -euo pipefail
        deadline=$(( $(date +%s) + 70 ))
        ready=0
        while [ "$(date +%s)" -lt "$deadline" ]; do
          if timeout 10 k3s ctr -n k8s.io snapshots --snapshotter devmapper ls >/dev/null 2>&1; then
            ready=1
            break
          fi
          sleep 1
        done
        if [ "$ready" != 1 ]; then
          echo "voie-devmapper-gc: containerd snapshotter not reachable; not blocking Fabric" >&2
          exit 0
        fi
        removed=0
        probes=0
        while [ "$(date +%s)" -lt "$deadline" ]; do
          pass=0
          # Drop the tabwriter header. Try Active rows first so a sandbox
          # holding a ghost parent can release it. Time-box the listing:
          # an unbounded ctr hang kept pause/fabricd dead after reboot.
          listing="$(timeout 15 k3s ctr -n k8s.io snapshots --snapshotter devmapper ls 2>/dev/null | awk 'NR > 1 { print $1, $3 }' || true)"
          while read -r key _kind; do
            [ -n "$key" ] || continue
            remaining=$(( deadline - $(date +%s) ))
            [ "$remaining" -ge 2 ] || break
            probes=$((probes + 1))
            [ "$probes" -le 32 ] || break
            set +e
            err="$(timeout 5 k3s ctr -n k8s.io snapshots --snapshotter devmapper prepare --mounts "voie-gc-$$" "$key" 2>&1)"
            probe_rc=$?
            set -e
            timeout 5 k3s ctr -n k8s.io snapshots --snapshotter devmapper rm "voie-gc-$$" >/dev/null 2>&1 || true
            [ "$probe_rc" -eq 0 ] && continue
            case "$err" in
              *"device metadata not found"*|*"Failed to find device"*|*"Failed to get device"*)
                if timeout 5 k3s ctr -n k8s.io snapshots --snapshotter devmapper rm "$key" >/dev/null 2>&1; then
                  echo "voie-devmapper-gc: removed ghost $key"
                  pass=$((pass + 1))
                  removed=$((removed + 1))
                fi
                ;;
            esac
          done <<EOF
        $listing
        EOF
          [ "$pass" -gt 0 ] || break
        done
        echo "voie-devmapper-gc: removed $removed ghost snapshot(s)"
      '';
    };
  };

  systemd.services.voie-devmapper-pause = {
    description = "Put /pause on the Firecracker devmapper snapshot of the CRI sandbox image";
    wantedBy = [ "multi-user.target" ];
    after = [
      "k3s.service"
      "voie-pause-image-load.service"
      "voie-devmapper-gc.service"
    ];
    requires = [
      "k3s.service"
      "voie-pause-image-load.service"
    ];
    wants = [ "voie-devmapper-gc.service" ];
    before = [ "voie-fabricd.service" ];
    path = [
      pkgs.k3s
      pkgs.util-linux
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.gawk
      pkgs.jq
    ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      TimeoutStartSec = "180";
      ExecStart = pkgs.writeShellScript "voie-devmapper-pause" ''
        set -euo pipefail
        IMAGE=docker.io/rancher/mirrored-pause:3.6
        CRI_SOCK=unix:///run/k3s/containerd/containerd.sock
        FILL=/run/voie/pause-fill
        CHECK=/run/voie/pause-check
        mkdir -p /run/voie "$FILL" "$CHECK"

        # Safe teardown: every exit path leaves no mounted scratch space and
        # no transient devmapper snapshot behind. Success paths have already
        # cleaned up, making these deliberate no-ops; failure paths must not
        # leak a held thin volume into the next attempt.
        cleanup() {
          local rc=$?
          trap - EXIT INT TERM
          umount "$CHECK" 2>/dev/null || true
          umount "$FILL" 2>/dev/null || true
          timeout 5 k3s ctr -n k8s.io snapshots --snapshotter devmapper rm voie-pause-check 2>/dev/null || true
          timeout 5 k3s ctr -n k8s.io snapshots --snapshotter devmapper rm voie-pause-fill 2>/dev/null || true
          exit "$rc"
        }
        trap cleanup EXIT
        trap 'exit 130' INT
        trap 'exit 143' TERM

        log() { echo "voie-devmapper-pause: $*"; }

        # pipefail-safe tag probe: the listing is captured to completion
        # BEFORE matching, so the fixed-substring check can never SIGPIPE a
        # live ctr mid-listing and invert a match into pipeline failure.
        tag_present() {
          local listing
          listing="$(k3s ctr -n k8s.io images ls 2>/dev/null)" || return 1
          [[ "$listing" == *"$1"* ]]
        }

        # Bounded readiness probes: a wedged containerd must fail this unit
        # within its TimeoutStartSec instead of hanging one ctr call forever.
        deadline=$(( $(date +%s) + 60 ))
        ready=0
        while [ "$(date +%s)" -lt "$deadline" ]; do
          if timeout 10 k3s ctr -n k8s.io images ls >/dev/null 2>&1; then
            ready=1
            break
          fi
          sleep 1
        done
        if [ "$ready" != 1 ]; then
          echo "containerd did not become reachable" >&2
          exit 1
        fi
        while ! tag_present "$IMAGE"; do
          if [ "$(date +%s)" -ge "$deadline" ]; then
            echo "pause image $IMAGE missing from containerd" >&2
            exit 1
          fi
          sleep 2
        done

        # CRI chain ID is the key kata Prepare() resolves. Do not
        # `images mount` onto a fixed overlayfs path: a leaked View at
        # that path fails with "bucket already exists" before this unit
        # can adopt an already-seeded devmapper chain.
        KEY="$(k3s crictl --runtime-endpoint "$CRI_SOCK" inspecti "$IMAGE" 2>/dev/null | jq -r '.info.chainID // empty' || true)"
        if [ -z "$KEY" ]; then
          echo "pause image chain ID not found" >&2
          exit 1
        fi
        log "CRI chain key $KEY"

        # A Ready kata sandbox whose snapshot parent is KEY already execs
        # /pause from this chain. Preparing a new child against a parent
        # that already has Active children hangs in the snapshotter; that
        # hang is not evidence that /pause is missing.
        chain_in_use_by_ready_kata() {
          local pods child prefix
          pods="$(timeout 10 k3s crictl --runtime-endpoint "$CRI_SOCK" pods --state Ready 2>/dev/null)" || return 1
          echo "$pods" | grep -q 'kata-fc-rs-voie' || return 1
          while IFS= read -r child; do
            [ -n "$child" ] || continue
            prefix="$(printf '%s' "$child" | cut -c1-13)"
            [ -n "$prefix" ] || continue
            echo "$pods" | grep -q "$prefix" && return 0
          done < <(timeout 10 k3s ctr -n k8s.io snapshots --snapshotter devmapper ls 2>/dev/null | awk -v k="$KEY" 'NR>1 && $2==k && $3=="Active" {print $1}')
          return 1
        }
        if chain_in_use_by_ready_kata; then
          log "devmapper chain $KEY is already the parent of a Ready kata sandbox"
          exit 0
        fi

        # True when the committed devmapper chain already carries a runnable
        # /pause — either from our own earlier run or from the CRI's own
        # concurrent unpack. Gated on a pure-metadata `snapshots info` first:
        # preparing a probe against an absent chain would allocate a thin
        # device just to roll it back, and its deferred removal collides with
        # the next allocation ("file exists"). Leaves the probe snapshot
        # removed. k3s ctr has `info`, not Docker-style `snapshots stat`.
        chain_seeded() {
          k3s ctr -n k8s.io snapshots --snapshotter devmapper info "$KEY" >/dev/null 2>&1 || return 1
          # `prepare --mounts` prints the raw mount record as JSON straight
          # from the snapshotter. Deliberately NOT `snapshots mounts`: that
            # command routes through the daemon MountManager Activate RPC,
            # which k3s' embedded containerd does not serve.
          local json dev
          json="$(mktemp)" || return 1
          timeout 15 k3s ctr -n k8s.io snapshots --snapshotter devmapper prepare --mounts voie-pause-check "$KEY" >"$json" 2>/dev/null ||
            {
              timeout 5 k3s ctr -n k8s.io snapshots --snapshotter devmapper rm voie-pause-check >/dev/null 2>&1 || true
              rm -f "$json"
              return 1
            }
          dev="$(jq -r '.[0].Source // .[0].source // empty' "$json")"
          rm -f "$json"
          local ok=1
          if [ -n "''${dev:-}" ] && [ -e "$dev" ] &&
            mount -t ext4 -o ro "$dev" "$CHECK" 2>/dev/null &&
            test -x "$CHECK/pause"; then
            ok=0
          fi
          umount "$CHECK" 2>/dev/null || true
          timeout 5 k3s ctr -n k8s.io snapshots --snapshotter devmapper rm voie-pause-check >/dev/null 2>&1 || true
          return "$ok"
        }

        # Idempotent rerun: leave an existing good chain untouched.
        if chain_seeded; then
          log "devmapper chain $KEY already carries /pause"
          exit 0
        fi

        # DESTROY of the runtime pool leaves containerd snapshot metadata
        # pointing at thin devices that no longer exist. That ghost is
        # not shared usable state: prepare fails with a missing-device
        # error, so removing KEY is required before a fresh seed. An
        # unused empty chain (prepares, but has no /pause) is the same
        # class of leftover and is removed when no Ready kata holds it.
        if k3s ctr -n k8s.io snapshots --snapshotter devmapper info "$KEY" >/dev/null 2>&1; then
          set +e
          probe_err="$(timeout 15 k3s ctr -n k8s.io snapshots --snapshotter devmapper prepare --mounts voie-pause-check "$KEY" 2>&1)"
          probe_rc=$?
          set -e
          timeout 5 k3s ctr -n k8s.io snapshots --snapshotter devmapper rm voie-pause-check >/dev/null 2>&1 || true
          if [ "$probe_rc" -eq 0 ]; then
            # Device exists but chain_seeded already proved /pause is
            # missing. An unused empty chain is leftover from a pool
            # recreate or a CRI unpack onto overlayfs; it is not a live
            # sandbox. Delete it and fall through to seed. A Ready kata
            # parent is left untouched.
            if chain_in_use_by_ready_kata; then
              log "chain $KEY exists without /pause but a Ready kata sandbox holds it; refusing to mutate shared state" >&2
              exit 1
            fi
            log "removing unusable empty chain $KEY (no /pause, no Ready kata)"
            k3s ctr -n k8s.io snapshots --snapshotter devmapper rm "$KEY"
          else
            case "$probe_err" in
              *"device metadata not found"*|*"Failed to find device"*|*"Failed to get device"*)
                log "removing ghost devmapper snapshot $KEY"
                k3s ctr -n k8s.io snapshots --snapshotter devmapper rm "$KEY"
                ;;
              *)
                log "chain $KEY exists in devmapper metadata but carries no usable /pause; refusing to mutate shared state" >&2
                log "prepare: $probe_err" >&2
                exit 1
                ;;
            esac
          fi
        fi

        # Seed from an empty parent into the shared chain name while it is
        # still free. Each round uses a unit-owned UNIQUE snapshot name so a
        # previous round's deferred device removal can never collide with the
        # next allocation, and never touches shared state.
        #
        # The commit outcome is captured verbatim and classified, never
        # masked: containerd's metadata store rejects a duplicate commit with
        # bolt's "bucket already exists", surfaced raw or wrapped by the
        # grpc/error layers. That exact condition is a LOST RACE against the
        # concurrent CRI commit of the shared chain; anything else is a real
        # fault. Round codes: 0 committed; 1 local failure (prepare/mount);
        # 2 commit lost the already-exists race; 3 other commit error.
        commit_err=""
        seed_once() {
          local fill="$1"
          local json
          json="$(mktemp)" || return 1
          timeout 15 k3s ctr -n k8s.io snapshots --snapshotter devmapper prepare --mounts "$fill" "" >"$json" ||
            {
              log "seed: prepare failed (round with existing device held by CRI unpack?)" >&2
              return 1
            }
          {
            DEV="$(jq -r '.[0].Source // .[0].source // empty' "$json")"
            rm -f "$json"
            log "seed: device $DEV"
            test -n "''${DEV:-}" && test -e "$DEV" &&
              mount -t ext4 "$DEV" "$FILL" &&
              cp -a ${pauseBin}/bin/pause "$FILL/pause" &&
              chmod 0755 "$FILL/pause" &&
              test -x "$FILL/pause"
          } || {
            umount "$FILL" 2>/dev/null || true
            return 1
          }
          umount "$FILL" || return 1
          # If an unused empty chain already occupies KEY, drop it in the
          # same breath as commit so the CRI cannot recreate a blank ext4
          # during a 30s adoption wait.
          if k3s ctr -n k8s.io snapshots --snapshotter devmapper info "$KEY" >/dev/null 2>&1; then
            if chain_in_use_by_ready_kata; then
              log "seed: $KEY is held by a Ready kata sandbox; refusing to replace it" >&2
              return 3
            fi
            k3s ctr -n k8s.io snapshots --snapshotter devmapper rm "$KEY" >/dev/null 2>&1 || true
          fi
          if commit_err="$(k3s ctr -n k8s.io snapshots --snapshotter devmapper commit "$KEY" "$fill" 2>&1)"; then
            log "seed: commit ok"
            return 0
          fi
          log "seed: commit failed: $commit_err" >&2
          case "$commit_err" in
            *"already exists"*) return 2 ;;
            *) return 3 ;;
          esac
        }

        release_fill() {
          local fill="$1"
          umount "$FILL" 2>/dev/null || true
          k3s ctr -n k8s.io snapshots --snapshotter devmapper rm "$fill" >/dev/null 2>&1 || true
        }

        # Give the CRI sandbox-image pre-pull a bounded chance to finish
        # first: seeding against an actively-unpacking daemon is the one race
        # this unit cannot win by retrying alone.
        quiet=0
        for _ in $(seq 1 10); do
          # Count data rows, never match the tabwriter header by substring.
          if [ "$(k3s ctr -n k8s.io snapshots --snapshotter devmapper ls 2>/dev/null | tail -n +2 | wc -l)" -eq 0 ]; then
            quiet=$((quiet + 1))
          else
            quiet=0
          fi
          if [ "$quiet" -ge 2 ]; then
            break
          fi
          sleep 1
        done
        if chain_seeded; then
          log "CRI pre-pull seeded $KEY; adopting it"
          exit 0
        fi

        # Lost-race adoption probe: after a duplicate-commit rejection the
        # shared chain belongs to whoever won it (normally the CRI unpack),
        # possibly still mid-copy. Probe usability immediately, then within
        # a bounded window, and adopt ONLY a runnable chain. A chain that
        # stays present-but-unusable keeps the strict failure below; this
        # unit never deletes or overwrites the shared chain.
        await_chain_usable() {
          local adopt_deadline=$(( $(date +%s) + 30 ))
          while :; do
            if chain_seeded; then
              return 0
            fi
            [ "$(date +%s)" -lt "$adopt_deadline" ] || return 1
            sleep 2
          done
        }

        ok=0
        for _ in 1 2 3 4 5 6 7 8; do
          fill="voie-pause-fill-$RANDOM"
          src_rc=0
          seed_once "$fill" || src_rc=$?
          release_fill "$fill"
          case "$src_rc" in
            0)
              ok=1
              log "seeded /pause into devmapper chain $KEY"
              break
              ;;
            2)
              if chain_seeded; then
                ok=1
                log "concurrent CRI commit won the $KEY race; adopting its chain"
                break
              fi
              if chain_in_use_by_ready_kata; then
                log "lost commit race for $KEY and a Ready kata sandbox holds the unusable chain; failing closed" >&2
                break
              fi
              log "lost commit race for $KEY; removing unusable empty chain and retrying seed"
              k3s ctr -n k8s.io snapshots --snapshotter devmapper rm "$KEY" >/dev/null 2>&1 || true
              ;;
            3)
              log "unrecoverable commit error for $KEY; refusing further attempts" >&2
              break
              ;;
            *)
              # Local round failure (prepare/mount): back off, then retry
              # with a fresh unique fill name.
              sleep 2
              ;;
          esac
        done
        if [ "$ok" != 1 ]; then
          log "could not seed $KEY" >&2
          exit 1
        fi
      '';
    };
  };

  # Fixed guest images are imported from the Nix store after containerd is
  # ready — no registry, no host-side manual ctr invocation. Profile 0 keeps
  # voie-runner:c1; Profile 1 adds workspace/app/postgres/gateway profiles.
  systemd.services.voie-fabric-image-load = {
    description = "Load the fixed VOIE guest images into local containerd";
    wantedBy = [ "multi-user.target" ];
    after = [ "k3s.service" ];
    requires = [ "k3s.service" ];
    path = [
      pkgs.coreutils
      pkgs.k3s
    ];
    script = ''
      set -euo pipefail
      deadline=$(( $(date +%s) + 1200 ))

      ready=0
      while [ "$(date +%s)" -lt "$deadline" ]; do
        if timeout 10 k3s ctr -n k8s.io images ls >/dev/null 2>&1; then
          ready=1
          break
        fi
        sleep 1
      done
      if [ "$ready" != 1 ]; then
        echo "containerd did not become reachable" >&2
        exit 1
      fi

      tag_present() {
        local listing
        listing="$(k3s ctr -n k8s.io images ls 2>/dev/null)" || return 1
        [[ "$listing" == *"$1"* ]]
      }

      # Skip only when this exact Nix store tarball is already imported.
      # A tag match alone is not enough: DESTROY and generation switches
      # keep voie-workspace:v1 while the tarball (and /bin/voie-pack) move.
      stamp_dir=/var/lib/voie-fabricd/image-stamps
      mkdir -p "$stamp_dir"

      import_one() {
        local tar="$1"
        local tag="$2"
        local stamp="$stamp_dir/$(printf '%s' "$tag" | tr '/:' '__')"
        local tar_bytes import_secs remaining attempt
        tar_bytes="$(stat -c %s "$tar")"
        import_secs=$(( tar_bytes / (20 * 1024 * 1024) + 30 ))
        if [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$tar" ] && tag_present "$tag"; then
          echo "voie-fabric-image-load: $tag already $tar"
          return 0
        fi
        echo "voie-fabric-image-load: importing $tag from $tar"
        while [ "$(date +%s)" -lt "$deadline" ]; do
          remaining=$(( deadline - $(date +%s) ))
          attempt=$import_secs
          [ "$attempt" -gt "$remaining" ] && attempt=$remaining
          [ "$attempt" -ge 1 ] || break
          timeout "$attempt" k3s ctr -n k8s.io images import "$tar" >/dev/null 2>&1 || true
          if tag_present "$tag"; then
            printf '%s\n' "$tar" > "$stamp"
            return 0
          fi
          sleep 1
        done
        echo "containerd did not accept the fixed image $tag within deadline" >&2
        return 1
      }

      import_one ${runnerImage} voie-runner:c1
      import_one ${workspaceImage} voie-workspace:v1
      import_one ${appImage} voie-app:v1
      import_one ${postgresImage} voie-postgres:v1
      import_one ${gatewayImage} voie-gateway:v1
    '';
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      TimeoutStartSec = "1800s";
    };
  };

  systemd.services.voie-fabricd = {
    description = "VOIE fabric daemon";
    wantedBy = [ "multi-user.target" ];
    after = [
      "network-online.target"
      "k3s.service"
      "voie-devmapper-pause.service"
      "voie-fabric-image-load.service"
    ];
    wants = [
      "network-online.target"
      "voie-devmapper-pause.service"
    ];
    requires = [
      "voie-fabric-image-load.service"
    ];
    path = [
      pkgs.k3s
      pkgs.kubectl
      pkgs.lvm2
      pkgs.thin-provisioning-tools
      pkgs.util-linux
      pkgs.e2fsprogs
      pkgs.cryptsetup
      pkgs.coreutils
    ];
    serviceConfig = {
      Type = "simple";
      User = "root";
      Group = "root";
      EnvironmentFile = "/etc/voie/fabric.env";
      ExecStart = "${pkgs.voie-fabricd}/bin/voie-fabricd";
      # k3s restart is a SIGTERM of dependents and of this unit when operators
      # bounce the node; on-failure would leave Fabric dark after a clean stop.
      Restart = "always";
      RestartSec = "3s";
      # Exit 2 is fabricd's controlled refusal on missing/invalid
      # VOIE_FABRIC_CLIENT_SHA256: keep the unit failed (fail-closed, visible)
      # but do not restart-loop it; genuine operational failures still restart.
      RestartPreventExitStatus = 2;
      NoNewPrivileges = true;
      ProtectHome = true;
      PrivateTmp = true;
      # LVM workspace volumes, kubelet, and the jailer live outside
      # /var/lib/voie-fabricd. ProtectSystem=strict blocked lvcreate.
      ReadWritePaths = [
        "/var/lib/voie-fabricd"
        "/var/lib/voie"
        "/var/lib/rancher"
        "/etc/lvm"
        "/run"
        "/dev"
      ];
    };
  };
}
