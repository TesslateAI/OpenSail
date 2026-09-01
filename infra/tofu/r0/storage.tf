resource "azurerm_storage_account" "blob" {
  name                            = "${replace(var.name_prefix, "-", "")}${random_string.suffix.result}"
  resource_group_name             = azurerm_resource_group.r0.name
  location                        = azurerm_resource_group.r0.location
  account_tier                    = "Standard"
  account_replication_type        = "ZRS"
  account_kind                    = "StorageV2"
  min_tls_version                 = "TLS1_2"
  https_traffic_only_enabled      = true
  allow_nested_items_to_be_public = false
  shared_access_key_enabled       = true
  local_user_enabled              = false

  blob_properties {
    versioning_enabled = false
    delete_retention_policy {
      days = 14
    }
    container_delete_retention_policy {
      days = 14
    }
  }
}

resource "azurerm_storage_container" "dsh" {
  name                  = "dsh-events"
  storage_account_id    = azurerm_storage_account.blob.id
  container_access_type = "private"
}

resource "azurerm_storage_management_policy" "blob" {
  storage_account_id = azurerm_storage_account.blob.id

  rule {
    name    = "logs-expire"
    enabled = true
    filters {
      prefix_match = ["dsh-events/logs/"]
      blob_types   = ["blockBlob"]
    }
    actions {
      base_blob {
        delete_after_days_since_modification_greater_than = 14
      }
    }
  }

  rule {
    name    = "archives-cool"
    enabled = true
    filters {
      prefix_match = ["dsh-events/backups/"]
      blob_types   = ["blockBlob"]
    }
    actions {
      base_blob {
        tier_to_cool_after_days_since_modification_greater_than = 30
      }
    }
  }
}
