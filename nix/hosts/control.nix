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

  fileSystems."/" = lib.mkDefault {
    device = "/dev/disk/by-label/nixos";
    fsType = "ext4";
  };

  boot.loader.grub.enable = lib.mkDefault true;
  boot.loader.grub.device = lib.mkDefault "nodev";

  # Cheap current Azure SKUs are NVMe-only. azure-common.nix still loads
  # hv_storvsc for SCSI, so the initrd must also see nvme or the VM cannot
  # find the OS disk.
  boot.initrd.kernelModules = [
    "nvme"
    "pci-hyperv"
    "pci-hyperv-intf"
  ];
}
