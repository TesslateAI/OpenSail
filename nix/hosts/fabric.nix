{ ... }:
let
  # NixOS-side names for this host. Debian rescue swaps nvme device order;
  # by-id/by-uuid stay put. GRUB is BIOS on the OS disk, not the Fabric VG.
  osDisk = "/dev/disk/by-id/nvme-SAMSUNG_MZVL2512HCJQ-00B00_S675NX0T351361";
  rootUuid = "984aaaa8-f4b7-4ae6-ae37-9ec45cda467c";
in
{
  imports = [
    ../modules/fabric.nix
    ../runtime/kata-runtime-rs.nix
  ];

  voie.kataRuntimeRs.enable = true;

  nixpkgs.hostPlatform = "x86_64-linux";
  system.stateVersion = "26.05";
  networking.hostName = "baremetal-1";

  # Same operator key live sshd already trusts. The shared fabric module
  # turns on initrd OpenSSH on 2222 only when root/voie keys exist in Nix;
  # without this, a leftover-LV hang still has no rescue listener after
  # nixos-rebuild.
  users.users.root.openssh.authorizedKeys.keys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAXaurWAGQYaubPTMojakdlwoL0mn9b+j9VlF9qQ0uyt"
  ];
  users.users.voie.openssh.authorizedKeys.keys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAXaurWAGQYaubPTMojakdlwoL0mn9b+j9VlF9qQ0uyt"
  ];

  fileSystems."/" = {
    device = "/dev/disk/by-uuid/${rootUuid}";
    fsType = "ext4";
  };

  # BIOS GRUB on the OS disk. Leaving grub off meant `nixos-rebuild switch`
  # swapped /run/current-system but left grub.cfg on the overlay generation,
  # so the next reboot lost the systemd overlay and hung on ws-root fstab.
  boot.loader.systemd-boot.enable = false;
  boot.loader.efi.canTouchEfiVariables = false;
  boot.loader.grub.enable = true;
  boot.loader.grub.device = osDisk;
  boot.loader.grub.configurationLimit = 8;

  # Bring enp41s0 up in initrd without baking the estate address into the
  # flake. Userspace DHCP remains after local-fs.
  boot.kernelParams = [
    "ip=::::baremetal-1:enp41s0:dhcp"
  ];

  networking.useDHCP = true;
  networking.interfaces.enp41s0.useDHCP = true;

  nix.settings.experimental-features = [
    "nix-command"
    "flakes"
  ];

  # Same k3s flags Ansible writes. The generation must carry this file so
  # a rebuild does not depend on a leftover ansible copy under /etc/voie.
  environment.etc."voie/k3s/config.yaml".text = ''
    cluster-init: true
    write-kubeconfig-mode: "0600"
    disable:
      - traefik
      - servicelb
      - local-storage
    flannel-backend: none
    disable-network-policy: true
    secrets-encryption: true
  '';
}
