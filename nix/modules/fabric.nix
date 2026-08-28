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
  runnerImage = pkgs.callPackage ../runtime/voie-runner-image.nix { };
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

  boot.kernelParams = [ "cgroup_enable=memory" ];

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
    pkgs.util-linux
    pkgs.iptables
    pkgs.openssl
    pkgs.curl
    pkgs.jq
    pkgs.tailscale
  ];

  environment.etc."voie/guest-rootfs".source = "${pkgs.voie-guest-rootfs}/rootfs.squashfs";

  systemd.mounts = [
    {
      what = "/dev/voie-ws/ws-root";
      where = "/var/lib/voie/workspaces";
      type = "ext4";
      options = "defaults";
      wantedBy = [ "multi-user.target" ];
    }
  ];

  systemd.tmpfiles.rules = [
    "d /etc/voie 0750 root root -"
    "d /etc/voie/secrets 0700 root root -"
    "d /etc/voie/k3s 0750 root root -"
    "d /etc/voie/k8s 0750 root root -"
    "d /var/lib/voie-fabricd 0750 voie-fabricd voie-fabricd -"
    "d /var/lib/voie/workspaces 0750 root root -"
    "d /var/lib/rancher/k3s/agent/etc/containerd 0750 root root -"
    "L+ /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl - - - - /etc/voie/k3s/containerd-config.toml.tmpl"
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
      pool_name = "voie--ws-workspaces-tpool"
      root_path = "/var/lib/rancher/k3s/agent/containerd/io.containerd.snapshotter.v1.devmapper"
      base_image_size = "10GB"
      async_remove = false
  '';

  systemd.services.dropbear-rescue = {
    description = "Dropbear rescue SSH on TCP/2222";
    wantedBy = [ "multi-user.target" ];
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    serviceConfig = {
      Type = "simple";
      ExecStartPre = [
        "${pkgs.coreutils}/bin/mkdir -p /var/lib/dropbear /root/.ssh /etc/dropbear"
        "${pkgs.bash}/bin/bash -c 'test -e /var/lib/dropbear/dropbear_ed25519_host_key || ${pkgs.dropbear}/bin/dropbearkey -t ed25519 -f /var/lib/dropbear/dropbear_ed25519_host_key'"
        "${pkgs.bash}/bin/bash -c '${pkgs.coreutils}/bin/install -m 600 /etc/ssh/authorized_keys.d/root /root/.ssh/authorized_keys; ${pkgs.coreutils}/bin/install -m 600 /etc/ssh/authorized_keys.d/root /etc/dropbear/authorized_keys'"
      ];
      ExecStart = "${pkgs.dropbear}/bin/dropbear -F -E -p 2222 -s -g -r /var/lib/dropbear/dropbear_ed25519_host_key";
      Restart = "always";
      RestartSec = "2s";
    };
  };

  systemd.services.k3s = {
    description = "K3s server for the VOIE Firecracker fabric";
    wantedBy = [ "multi-user.target" ];
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    path = [
      pkgs.k3s
      pkgs.iptables
      pkgs.util-linux
      pkgs.lvm2
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
      TimeoutStartSec = "0";
      Restart = "always";
      RestartSec = "5s";
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

  systemd.services.voie-devmapper-pause = {
    description = "Put /pause on the Firecracker devmapper snapshot of the CRI sandbox image";
    wantedBy = [ "multi-user.target" ];
    after = [
      "k3s.service"
      "voie-pause-image-load.service"
    ];
    requires = [
      "k3s.service"
      "voie-pause-image-load.service"
    ];
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
        SRC=/run/voie/pause-overlay
        FILL=/run/voie/pause-fill
        CHECK=/run/voie/pause-check
        mkdir -p /run/voie "$SRC" "$FILL" "$CHECK"

        # Safe teardown: every exit path leaves no mounted scratch space and
        # no transient devmapper snapshot behind. Success paths have already
        # cleaned up, making these deliberate no-ops; failure paths must not
        # leak a held thin volume into the next attempt.
        cleanup() {
          local rc=$?
          trap - EXIT INT TERM
          umount "$CHECK" 2>/dev/null || true
          umount "$FILL" 2>/dev/null || true
          umount "$SRC" 2>/dev/null || true
          k3s ctr -n k8s.io snapshots --snapshotter devmapper rm voie-pause-check 2>/dev/null || true
          k3s ctr -n k8s.io snapshots --snapshotter devmapper rm voie-pause-fill 2>/dev/null || true
          k3s ctr -n k8s.io images unmount "$SRC" 2>/dev/null || true
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
        umount "$SRC" 2>/dev/null || true
        k3s ctr -n k8s.io images unmount "$SRC" 2>/dev/null || true
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

        k3s ctr -n k8s.io images mount --snapshotter overlayfs "$IMAGE" "$SRC"
        test -x "$SRC/pause"
        # `ctr images mount` registers the active overlayfs snapshot under the
        # mount target as its key; the PARENT column of that row is the image's
        # full chain ID — exactly the key a kata devmapper Prepare() resolves.
        KEY=$(k3s ctr -n k8s.io snapshots --snapshotter overlayfs ls | awk -v m="$SRC" '$1 == m { print $2 }')
        test -n "$KEY"
        log "overlayfs chain key $KEY"

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
          k3s ctr -n k8s.io snapshots --snapshotter devmapper prepare --mounts voie-pause-check "$KEY" >"$json" 2>/dev/null ||
            {
              k3s ctr -n k8s.io snapshots --snapshotter devmapper rm voie-pause-check >/dev/null 2>&1 || true
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
          k3s ctr -n k8s.io snapshots --snapshotter devmapper rm voie-pause-check >/dev/null 2>&1 || true
          return "$ok"
        }

        # Idempotent rerun: leave an existing good chain untouched.
        if chain_seeded; then
          log "devmapper chain $KEY already carries /pause"
          exit 0
        fi

        # A present chain without usable content is shared state this unit
        # must never delete or overwrite: other consumers own it too.
        if k3s ctr -n k8s.io snapshots --snapshotter devmapper info "$KEY" >/dev/null 2>&1; then
          log "chain $KEY exists in devmapper metadata but carries no usable /pause; refusing to mutate shared state" >&2
          exit 1
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
          k3s ctr -n k8s.io snapshots --snapshotter devmapper prepare --mounts "$fill" "" >"$json" ||
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
              cp -a "$SRC/pause" "$FILL/pause" &&
              test -x "$FILL/pause"
          } || {
            umount "$FILL" 2>/dev/null || true
            return 1
          }
          umount "$FILL" || return 1
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
        for _ in 1 2 3; do
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
              # Lost the commit race: bounded adoption of the winner's
              # runnable chain; never mutate shared state with more commits.
              if await_chain_usable; then
                ok=1
                log "concurrent CRI commit won the $KEY race; adopting its chain"
              else
                log "lost commit race for $KEY and chain stayed present-but-unusable; failing closed" >&2
              fi
              break
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

  # The fixed guest image is imported from the Nix store after containerd is
  # ready — no registry, no host-side manual ctr invocation. This is a shared,
  # production-guaranteed boot chain: every fabric profile imports the exact
  # packaged runner image before the daemon that schedules it may start.
  systemd.services.voie-fabric-image-load = {
    description = "Load the fixed VOIE runner image into local containerd";
    wantedBy = [ "multi-user.target" ];
    after = [ "k3s.service" ];
    requires = [ "k3s.service" ];
    path = [
      pkgs.coreutils
      pkgs.k3s
    ];
    script = ''
      set -euo pipefail
      IMAGE=voie-runner:c1
      deadline=$(( $(date +%s) + 240 ))

      # Bounded readiness probe: a wedged containerd must fail the unit, not
      # consume its whole timeout inside one hung ctr call.
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


      # pipefail-safe tag probe: the listing is captured to completion BEFORE
      # matching. `grep -qF` on a live ctr pipe closes the read end at the
      # first match while ctr still writes later rows, killing ctr with
      # SIGPIPE (exit 141); with pipefail that turns a successful match into
      # pipeline failure and the caller loops forever.
      tag_present() {
        local listing
        listing="$(k3s ctr -n k8s.io images ls 2>/dev/null)" || return 1
        [[ "$listing" == *"$1"* ]]
      }

      # Import attempts are individually time-boxed; success is proven by the
      # exact tag being present, never by the exit status alone. The
      # per-attempt budget scales with the tar size (>=20 MiB/s sustained
      # unpack floor, 30s minimum) so a real first import completes instead
      # of being killed mid-import every round; the overall deadline still
      # bounds the unit, and each attempt is capped at the time remaining.
      tar_bytes="$(stat -c %s '${runnerImage}')"
      import_secs=$(( tar_bytes / (20 * 1024 * 1024) + 30 ))
      while [ "$(date +%s)" -lt "$deadline" ]; do
        remaining=$(( deadline - $(date +%s) ))
        attempt=$import_secs
        [ "$attempt" -gt "$remaining" ] && attempt=$remaining
        [ "$attempt" -ge 1 ] || break
        if timeout "$attempt" k3s ctr -n k8s.io images import ${runnerImage} >/dev/null 2>&1 &&
          tag_present "$IMAGE"; then
          exit 0
        fi
        sleep 1
      done
      echo "containerd did not accept the fixed runner image $IMAGE within deadline" >&2
      exit 1
    '';
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      TimeoutStartSec = "300s";
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
    wants = [ "network-online.target" ];
    requires = [
      "voie-devmapper-pause.service"
      "voie-fabric-image-load.service"
    ];
    path = [
      pkgs.k3s
      pkgs.kubectl
      pkgs.lvm2
      pkgs.util-linux
      pkgs.e2fsprogs
      pkgs.coreutils
    ];
    serviceConfig = {
      Type = "simple";
      User = "root";
      Group = "root";
      EnvironmentFile = "/etc/voie/fabric.env";
      ExecStart = "${pkgs.voie-fabricd}/bin/voie-fabricd";
      Restart = "on-failure";
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
