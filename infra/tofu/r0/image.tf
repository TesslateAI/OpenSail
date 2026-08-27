resource "azurerm_storage_account" "images" {
  name                            = "${replace(var.name_prefix, "-", "")}img${random_string.suffix.result}"
  resource_group_name             = azurerm_resource_group.r0.name
  location                        = azurerm_resource_group.r0.location
  account_tier                    = "Standard"
  account_replication_type        = "LRS"
  account_kind                    = "StorageV2"
  min_tls_version                 = "TLS1_2"
  https_traffic_only_enabled      = true
  allow_nested_items_to_be_public = false
  shared_access_key_enabled       = true
  local_user_enabled              = false
}

resource "azurerm_storage_container" "vhds" {
  name                  = "vhds"
  storage_account_id    = azurerm_storage_account.images.id
  container_access_type = "private"
}

resource "azurerm_storage_blob" "control_vhd" {
  count = var.control_image_vhd_path != "" ? 1 : 0

  parallelism            = 1
  name                   = "nixos-control-boot.vhd"
  storage_account_name   = azurerm_storage_account.images.name
  storage_container_name = azurerm_storage_container.vhds.name
  type                   = "Page"
  source                 = var.control_image_vhd_path
}

resource "azurerm_image" "control" {
  count = var.control_image_vhd_path != "" ? 1 : 0

  name                = "${var.name_prefix}-control-nixos"
  location            = azurerm_resource_group.r0.location
  resource_group_name = azurerm_resource_group.r0.name
  hyper_v_generation  = "V2"
  zone_resilient      = false

  os_disk {
    os_type      = "Linux"
    os_state     = "Generalized"
    blob_uri     = azurerm_storage_blob.control_vhd[0].url
    size_gb      = 10
    caching      = "ReadWrite"
    storage_type = "Standard_LRS"
  }

  lifecycle {
    replace_triggered_by = [azurerm_storage_blob.control_vhd[0].id]
  }
}

resource "azurerm_shared_image_gallery" "r0" {
  name                = "${replace(var.name_prefix, "-", "")}gal${random_string.suffix.result}"
  location            = azurerm_resource_group.r0.location
  resource_group_name = azurerm_resource_group.r0.name
}

resource "azurerm_shared_image" "control" {
  name                                = "control"
  gallery_name                        = azurerm_shared_image_gallery.r0.name
  resource_group_name                 = azurerm_resource_group.r0.name
  location                            = azurerm_resource_group.r0.location
  os_type                             = "Linux"
  hyper_v_generation                  = "V2"
  architecture                        = "x64"
  disk_controller_type_nvme_enabled   = true
  accelerated_network_support_enabled = false

  identifier {
    publisher = "voie"
    offer     = "voie-cloud"
    sku       = "control"
  }
}

resource "azurerm_shared_image_version" "control" {
  count = var.control_image_vhd_path != "" ? 1 : 0

  name                = "0.0.4"
  gallery_name        = azurerm_shared_image_gallery.r0.name
  image_name          = azurerm_shared_image.control.name
  resource_group_name = azurerm_resource_group.r0.name
  location            = azurerm_resource_group.r0.location
  managed_image_id    = azurerm_image.control[0].id

  target_region {
    name                   = azurerm_resource_group.r0.location
    regional_replica_count = 1
    storage_account_type   = "Standard_LRS"
  }
}
