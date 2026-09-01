{
  config,
  lib,
  pkgs,
  ...
}:
{
  users.groups.voie-cloud = { };
  users.groups.headscale = { };
  users.groups.voie-activation = { };

  users.users.voie = {
    isNormalUser = true;
    extraGroups = [ "wheel" ];
  };

  users.users.voie-cloud = {
    isSystemUser = true;
    group = "voie-cloud";
    home = "/var/lib/voie-cloud";
    createHome = true;
  };

  users.users.headscale = {
    isSystemUser = true;
    group = "headscale";
    home = "/var/lib/headscale";
    createHome = true;
  };

  # Dedicated, credential-free identity for DSH activation children. It owns
  # no secrets: /etc/voie/control.env and /etc/voie/secrets are root:voie-cloud
  # and unreadable to this account. The control service reaches it only through
  # the FD-transfer broker below; there is no setuid path and no shared login.
  users.users.voie-activation = {
    isSystemUser = true;
    group = "voie-activation";
    home = "/var/lib/voie-activation";
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

  networking.firewall.enable = true;
  networking.firewall.allowedTCPPorts = [
    22
    443
  ];
  networking.firewall.allowedUDPPorts = [
    41641
    3478
  ];
  networking.firewall.trustedInterfaces = [ "tailscale0" ];

  services.tailscale.enable = true;

  environment.systemPackages = [
    pkgs.voie-cloud
    pkgs.python3 # Ansible managed-host interpreter
    pkgs.headscale
    pkgs.caddy
    pkgs.lego
    pkgs.openssl
    pkgs.curl
    pkgs.postgresql
    pkgs.jq
    pkgs.tailscale
    pkgs.nodejs
  ];

  environment.etc."voie/web-root".source = "${pkgs.voie-web}/share/voie-web";

  systemd.tmpfiles.rules = [
    "d /etc/voie 0751 root root -"
    "d /etc/voie/secrets 0750 root voie-cloud -"
    "d /etc/voie/certs 0755 root root -"
    "d /var/lib/voie-cloud 0750 voie-cloud voie-cloud -"
    "d /var/lib/headscale 0750 headscale headscale -"
    # Broker endpoint: dialable only by the control service group.
    "d /run/voie 0755 root root -"
    "d /run/voie/activation 0750 root voie-cloud -"
    # Per-child scratch homes; aged out after children are gone.
    "d /var/lib/voie-activation/home 0700 voie-activation voie-activation -"
    "r! /var/lib/voie-activation/home/voie-act-* - - - - 1h"
  ];

  # Ansible writes /etc/voie/hosts with the runtime Headscale IPv4 for
  # DNS:baremetal-1 (MagicDNS is off; the Fabric cert is that name). A
  # one-shot bind mount would vanish on reboot; this oneshot re-applies it.
  # A systemd .mount unit cannot target /etc/hosts on NixOS because that
  # path is a store symlink ("not canonical").
  systemd.services.voie-hosts-overlay = {
    description = "VOIE fabric hostname overlay";
    wantedBy = [ "multi-user.target" ];
    after = [ "local-fs.target" ];
    before = [
      "network-online.target"
      "nss-lookup.target"
      "voie-cloud.service"
    ];
    unitConfig.ConditionPathExists = "/etc/voie/hosts";
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = pkgs.writeShellScript "voie-hosts-overlay" ''
        set -euo pipefail
        source="$(${pkgs.util-linux}/bin/findmnt -n -o SOURCE /etc/hosts 2>/dev/null || true)"
        case "$source" in
          */etc/voie/hosts)
            if [[ "$source" != *deleted* ]]; then
              exit 0
            fi
            ;;
        esac
        ${pkgs.util-linux}/bin/umount /etc/hosts 2>/dev/null || true
        ${pkgs.util-linux}/bin/mount --bind /etc/voie/hosts /etc/hosts
      '';
    };
  };

  # Activation child handoff: the control process execs
  # voie-activation-launch as "node" (VOIE_NODE below); the launcher dials
  # this socket and passes exactly one descriptor — the parent<->child
  # bridge — plus the Nix store entry path. The per-connection broker
  # instance runs as voie-activation and execs the pinned Node.js binary,
  # so the DSH child executes with a real separate UID while the parent
  # keeps its wait()/kill supervision through the launcher. No setuid, no
  # sudo, no capabilities: if the socket or broker is missing, the launcher
  # exits nonzero and activation fails closed.
  systemd.sockets.voie-activation-broker = {
    description = "Activation child handoff socket";
    wantedBy = [ "sockets.target" ];
    before = [ "sockets.target" ];
    socketConfig = {
      ListenStream = "/run/voie/activation/spawn.sock";
      # inetd-style: each accepted connection spawns one
      # voie-activation-broker@.service instance with the connection fd
      # on stdin; the listener itself never leaks to children.
      Accept = true;
      SocketMode = "0660";
      SocketUser = "root";
      SocketGroup = "voie-cloud";
      RemoveOnStop = true;
    };
  };

  systemd.services."voie-activation-broker@" = {
    description = "Activation child executor as voie-activation";
    after = [ "voie-activation-broker.socket" ];
    requires = [ "voie-activation-broker.socket" ];
    serviceConfig = {
      # systemd hands each accepted connection to one instance on stdin.
      # --jitless keeps Node off executable memory so the pinned child is
      # compatible with MemoryDenyWriteExecute below; the boundary stays
      # FD-only and the store-entry check in the broker is unchanged.
      ExecStart = "${pkgs.voie-activation-handoff}/bin/voie-activation-broker ${pkgs.nodejs}/bin/node --jitless /run/current-system/sw/bin";
      User = "voie-activation";
      Group = "voie-activation";
      StandardInput = "socket";
      StandardOutput = "journal";
      StandardError = "null";
      NoNewPrivileges = true;
      ProtectSystem = "strict";
      ProtectHome = true;
      PrivateTmp = true;
      ReadWritePaths = [ "/var/lib/voie-activation" ];
      RestrictAddressFamilies = [ "AF_UNIX" ];
      CapabilityBoundingSet = [ ];
      LockPersonality = true;
      MemoryDenyWriteExecute = true;
      UMask = "0077";
    };
  };

  systemd.services.voie-cloud = {
    description = "VOIE Cloud control process";
    wantedBy = [ "multi-user.target" ];
    after = [
      "network-online.target"
      "voie-hosts-overlay.service"
    ];
    wants = [ "network-online.target" ];
    serviceConfig = {
      Type = "simple";
      User = "voie-cloud";
      Group = "voie-cloud";
      EnvironmentFile = "/etc/voie/control.env";
      # The prebuilt activation entry is pinned by the control generation
      # through pkgs.voie-activation-dist; VOIE_ACTIVATION_ENTRY is not
      # configurable per boot. VOIE_NODE points at the FD-transfer launcher
      # (not Node.js): the child then runs under voie-activation via the
      # socket-activated broker above, with an environment of exactly
      # HOME/LANG/PATH/TMPDIR and no access to control.env or secrets.
      Environment = [
        "VOIE_ACTIVATION_ENTRY=${pkgs.voie-activation-dist}/lib/voie-activation/dist/index.js"
        "VOIE_NODE=${pkgs.voie-activation-handoff}/bin/voie-activation-launch"
      ];
      ExecStart = "${pkgs.voie-cloud}/bin/voie-cloud";
      Restart = "on-failure";
      RestartSec = "3s";
      NoNewPrivileges = true;
      ProtectSystem = "strict";
      ProtectHome = true;
      PrivateTmp = true;
      ReadWritePaths = [ "/var/lib/voie-cloud" ];
    };
  };

  systemd.services.headscale = {
    description = "Headscale coordination server";
    wantedBy = [ "multi-user.target" ];
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    serviceConfig = {
      Type = "simple";
      User = "headscale";
      Group = "headscale";
      ExecStart = "${pkgs.headscale}/bin/headscale serve -c /etc/voie/headscale.yaml";
      Restart = "on-failure";
      RestartSec = "3s";
      StateDirectory = "headscale";
      RuntimeDirectory = "headscale";
      NoNewPrivileges = true;
      ProtectSystem = "strict";
      ProtectHome = true;
      PrivateTmp = true;
      ReadWritePaths = [ "/var/lib/headscale" ];
    };
  };

  systemd.services.voie-ingress = {
    description = "TLS ingress for voie-cloud and Headscale";
    wantedBy = [ "multi-user.target" ];
    after = [
      "network-online.target"
      "voie-cloud.service"
      "headscale.service"
    ];
    wants = [ "network-online.target" ];
    serviceConfig = {
      Type = "simple";
      User = "root";
      ExecStart = "${pkgs.caddy}/bin/caddy run --config /etc/voie/Caddyfile --adapter caddyfile";
      Restart = "on-failure";
      RestartSec = "3s";
      AmbientCapabilities = "CAP_NET_BIND_SERVICE";
      LimitNOFILE = "1048576";
    };
  };
}
