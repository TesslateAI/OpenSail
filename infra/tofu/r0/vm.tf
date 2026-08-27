resource "random_string" "suffix" {
  length  = 6
  upper   = false
  special = false
}

locals {
  # Count must be known at plan time. The managed image id is only known
  # after apply, so do not use it to decide whether the VM exists.
  control_has_image = var.control_image_vhd_path != "" || var.control_image_id != ""
  control_image_id  = try(azurerm_shared_image_version.control[0].id, var.control_image_id)
}

resource "azurerm_linux_virtual_machine" "control" {
  count = local.control_has_image ? 1 : 0

  name                            = "${var.name_prefix}-control"
  location                        = azurerm_resource_group.r0.location
  resource_group_name             = azurerm_resource_group.r0.name
  size                            = var.control_vm_size
  admin_username                  = var.admin_username
  disable_password_authentication = true
  network_interface_ids = [
    azurerm_network_interface.control.id,
  ]
  source_image_id      = local.control_image_id
  disk_controller_type = var.control_image_vhd_path != "" ? "NVMe" : null

  identity {
    type         = "UserAssigned"
    identity_ids = [azurerm_user_assigned_identity.control.id]
  }

  admin_ssh_key {
    username   = var.admin_username
    public_key = var.admin_ssh_public_key
  }

  os_disk {
    name                 = "${var.name_prefix}-control-os"
    caching              = "ReadWrite"
    storage_account_type = "Standard_LRS"
  }

  secure_boot_enabled = false
  vtpm_enabled        = false

  boot_diagnostics {
    storage_account_uri = azurerm_storage_account.images.primary_blob_endpoint
  }
}

resource "azurerm_managed_disk" "control_data" {
  name                 = "${var.name_prefix}-control-data"
  location             = azurerm_resource_group.r0.location
  resource_group_name  = azurerm_resource_group.r0.name
  storage_account_type = "Standard_LRS"
  create_option        = "Empty"
  disk_size_gb         = 32
}

resource "azurerm_virtual_machine_data_disk_attachment" "control_data" {
  count = local.control_has_image ? 1 : 0

  managed_disk_id    = azurerm_managed_disk.control_data.id
  virtual_machine_id = azurerm_linux_virtual_machine.control[0].id
  lun                = 0
  caching            = "ReadOnly"
}
