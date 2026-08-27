{ lib, ... }:
{
  imports = [
    ../modules/fabric.nix
    ../runtime/kata-runtime-rs.nix
  ];

  voie.kataRuntimeRs.enable = true;

  nixpkgs.hostPlatform = "x86_64-linux";
  system.stateVersion = "26.05";
  networking.hostName = "baremetal-1";

  # Generic label convention for the approved NixOS baseline. Live hosts keep
  # their local hardware-configuration and override these defaults.
  fileSystems."/" = lib.mkDefault {
    device = "/dev/disk/by-label/nixos";
    fsType = "ext4";
  };

  # The Fabric host is an already-booted, preinstalled NixOS baseline: its
  # bootloader is owned by whoever installed the system, not by this profile.
  # Keeping these off means `nixos-rebuild switch` only swaps the system
  # profile; activation never runs a systemd-boot install or touches EFI
  # variables, so no ESP mount is required on live hosts. Live hosts may still
  # override these weak defaults with their local hardware-configuration.
  boot.loader.systemd-boot.enable = lib.mkDefault false;
  boot.loader.efi.canTouchEfiVariables = lib.mkDefault false;
  # nixpkgs defaults grub.enable to true on non-containers; keep the profile
  # from falling back to it now that systemd-boot is off.
  boot.loader.grub.enable = lib.mkDefault false;
}
