data "azurerm_client_config" "current" {}

# User-secret references are opaque Key Vault secret names derived from the
# PostgreSQL `user_secrets.kv_name` convention. The namespace is isolated from
# infrastructure/bootstrap values in a separate vault.
locals {
  user_secret_name_prefix = "us-"
}

resource "azurerm_key_vault" "r0" {
  # New name: in-place RBAC→access-policy switches require
  # Microsoft.Authorization/roleAssignments/write, which Contributor lacks.
  name                          = "v0kva${random_string.suffix.result}"
  location                      = azurerm_resource_group.r0.location
  resource_group_name           = azurerm_resource_group.r0.name
  tenant_id                     = var.tenant_id
  sku_name                      = "standard"
  rbac_authorization_enabled    = false
  purge_protection_enabled      = false
  soft_delete_retention_days    = 7
  public_network_access_enabled = true
  enabled_for_deployment        = false
}

# Contributor cannot assign Azure RBAC on this vault. Access policies are the
# OpenTofu-owned data-plane grant; Key Vault remains the secret origin.
resource "azurerm_key_vault_access_policy" "deployer" {
  key_vault_id = azurerm_key_vault.r0.id
  tenant_id    = data.azurerm_client_config.current.tenant_id
  object_id    = data.azurerm_client_config.current.object_id

  secret_permissions = [
    "Get",
    "List",
    "Set",
    "Delete",
  ]
}

# User-secret material has a distinct vault boundary. The control identity can
# write, read one named secret for Fabric Environment binding injection, and
# delete by deterministic reference. It cannot list or read infrastructure
# vault secrets.
resource "azurerm_key_vault" "user_secrets" {
  name                          = "v0usk${random_string.suffix.result}"
  location                      = azurerm_resource_group.r0.location
  resource_group_name           = azurerm_resource_group.r0.name
  tenant_id                     = var.tenant_id
  sku_name                      = "standard"
  rbac_authorization_enabled    = false
  purge_protection_enabled      = false
  soft_delete_retention_days    = 7
  public_network_access_enabled = true
  enabled_for_deployment        = false
  tags = {
    "voie-user-secret-prefix" = local.user_secret_name_prefix
    "voie-user-secret-schema" = "user_secrets/kv_name/v1"
  }
}

resource "azurerm_key_vault_access_policy" "user_secrets_control" {
  key_vault_id = azurerm_key_vault.user_secrets.id
  tenant_id    = data.azurerm_client_config.current.tenant_id
  object_id    = azurerm_user_assigned_identity.control.principal_id

  secret_permissions = [
    "Get",
    "Set",
    "Delete",
  ]
}
