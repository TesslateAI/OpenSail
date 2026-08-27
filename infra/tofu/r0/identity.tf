resource "azurerm_user_assigned_identity" "control" {
  name                = "${var.name_prefix}-control"
  location            = azurerm_resource_group.r0.location
  resource_group_name = azurerm_resource_group.r0.name
}
