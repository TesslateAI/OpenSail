{
  modulesPath,
  lib,
  ...
}:
{
  imports = [
    "${modulesPath}/virtualisation/azure-common.nix"
    ../modules/control.nix
  ];

  nixpkgs.hostPlatform = "x86_64-linux";
  networking.hostName = "control";
  system.stateVersion = "26.05";
  virtualisation.azure.acceleratedNetworking = false;

  # Keep operator SSH after a generation switch. NixOS AuthorizedKeysFile
  # also reads /home/voie/.ssh; these store keys survive mutableUsers and
  # reboot. Same keys the live Control sshd already trusts.
  users.mutableUsers = true;
  users.users.voie.openssh.authorizedKeys.keys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILvYXFj42n7Q+sqfJCYY976oci3UHGUsjR5BsobtWuXf"
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAXaurWAGQYaubPTMojakdlwoL0mn9b+j9VlF9qQ0uyt"
  ];
  users.users.root.openssh.authorizedKeys.keys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOQ/NywDtccJfCLi5a3i8h3bmati89jBeSzyRcJUVGYw"
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAXaurWAGQYaubPTMojakdlwoL0mn9b+j9VlF9qQ0uyt"
  ];

  fileSystems."/" = lib.mkDefault {
    device = "/dev/disk/by-label/nixos";
    fsType = "ext4";
    autoResize = true;
  };
  fileSystems."/boot" = lib.mkDefault {
    device = "/dev/disk/by-label/ESP";
    fsType = "vfat";
  };

  boot.growPartition = lib.mkDefault true;
  boot.loader.grub.enable = lib.mkDefault true;
  boot.loader.grub.device = lib.mkDefault "nodev";
  boot.loader.grub.efiSupport = lib.mkDefault true;
  boot.loader.grub.efiInstallAsRemovable = lib.mkDefault true;

  # Cheap current Azure SKUs are NVMe-only. azure-common.nix still loads
  # hv_storvsc for SCSI, so the initrd must also see nvme or the VM cannot
  # find the OS disk.
  boot.initrd.kernelModules = [
    "nvme"
    "pci-hyperv"
    "pci-hyperv-intf"
  ];
}
