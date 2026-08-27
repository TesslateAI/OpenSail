{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:
{
  imports = [
    "${modulesPath}/virtualisation/azure-image.nix"
  ];

  virtualisation.azureImage.vmGeneration = "v2";
  virtualisation.diskSize = 10240;

  # Cheap current eastus SKUs are NVMe-only. azure-common.nix still loads
  # hv_storvsc for SCSI, so the initrd must also see nvme or the VM cannot
  # find the OS disk.
  # Azure NVMe OS disks are Hyper-V PCI devices. nvme.ko cannot bind until
  # pci-hyperv presents the controller; hv_storvsc only covers SCSI.
  boot.initrd.kernelModules = [
    "nvme"
    "pci-hyperv"
    "pci-hyperv-intf"
  ];

  # make-disk-image.nix defaults to 1024 MiB, which OOMs while copying this
  # control closure into the VHD. Pass memSize explicitly; the azure-image
  # module does not forward virtualisation.memorySize.
  system.build.azureImage = lib.mkForce (
    import "${modulesPath}/../lib/make-disk-image.nix" {
      name = "azure-image";
      inherit (config.image) baseName;
      format = "raw";
      postVM = ''
        # GPT ESP starts at 16384 sectors. A recovered qemu disk can have a
        # populated Nix store and still be unbootable if this partition is empty.
        ${lib.getExe pkgs.python3} -c "
        import sys
        with open('$diskImage', 'rb') as f:
            f.seek(16384 * 512)
            data = f.read(512)
        if data[510:512] != b'\\x55\\xaa':
            sys.exit('control azure image ESP is empty')
        "
        ${lib.getExe' pkgs.vmTools.qemu "qemu-img"} convert -f raw -o subformat=fixed,force_size -O vpc $diskImage $out/${config.image.fileName}
        rm $diskImage
      '';
      configFile = "${modulesPath}/virtualisation/azure-config-user.nix";
      bootSize = "${toString config.virtualisation.azureImage.bootSize}M";
      partitionTableType = "efi";
      inherit (config.virtualisation.azureImage) contents label;
      inherit (config.virtualisation) diskSize;
      inherit config lib pkgs;
      memSize = 8192;
      copyChannel = false;
    }
  );
}
