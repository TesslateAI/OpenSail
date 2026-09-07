resource "random_password" "postgres" {
  length  = 32
  special = false
}

resource "azurerm_postgresql_flexible_server" "control" {
  name                = "${var.name_prefix}-pg-${random_string.suffix.result}"
  resource_group_name = azurerm_resource_group.r0.name
  location            = var.location
  version             = var.postgres_version
  sku_name            = var.postgres_sku
  storage_mb          = var.postgres_storage_mb
  # VNet-injected into the delegated data subnet; reachable only over private
  # connectivity. A VNet-injected server must share its VNet's region, so it
  # follows the resource group location instead of a separate quota region.
  delegated_subnet_id           = azurerm_subnet.data.id
  private_dns_zone_id           = azurerm_private_dns_zone.postgres.id
  public_network_access_enabled = false
  administrator_login           = "voie"
  administrator_password        = random_password.postgres.result
  backup_retention_days         = 14

  authentication {
    password_auth_enabled = true
  }

  lifecycle {
    ignore_changes = [zone]
  }

  depends_on = [azurerm_private_dns_zone_virtual_network_link.postgres]
}

resource "azurerm_postgresql_flexible_server_database" "voie" {
  name      = "voie"
  server_id = azurerm_postgresql_flexible_server.control.id
  charset   = "UTF8"
  collation = "en_US.utf8"
}

resource "azurerm_postgresql_flexible_server_configuration" "require_tls" {
  name      = "require_secure_transport"
  server_id = azurerm_postgresql_flexible_server.control.id
  value     = "on"
}
