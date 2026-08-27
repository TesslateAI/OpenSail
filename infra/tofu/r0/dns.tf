data "cloudflare_zone" "public" {
  zone_id = var.cloudflare_zone_id
}

# Azure-owned private DNS zone for the Flexible Server. The provider requires
# this exact namespace for VNet-injected PostgreSQL Flexible Servers; Azure
# registers <server>.privatelink.postgres.database.azure.com here and only
# linked virtual networks resolve it, so the hostname has no public answer.
resource "azurerm_private_dns_zone" "postgres" {
  name                = "privatelink.postgres.database.azure.com"
  resource_group_name = azurerm_resource_group.r0.name
}

resource "azurerm_private_dns_zone_virtual_network_link" "postgres" {
  name                  = "${var.name_prefix}-pg-zone-link"
  resource_group_name   = azurerm_resource_group.r0.name
  private_dns_zone_name = azurerm_private_dns_zone.postgres.name
  virtual_network_id    = azurerm_virtual_network.r0.id
  registration_enabled  = false
}

locals {
  zone_name          = data.cloudflare_zone.public.name
  product_hostname   = var.public_hostname != "" ? var.public_hostname : var.base_domain
  headscale_hostname = "hs.${var.base_domain}"
  product_record = (
    local.product_hostname == local.zone_name
    ? "@"
    : trimsuffix(replace(local.product_hostname, ".${local.zone_name}", ""), ".")
  )
  headscale_record = trimsuffix(replace(local.headscale_hostname, ".${local.zone_name}", ""), ".")
}

resource "cloudflare_record" "product" {
  zone_id = var.cloudflare_zone_id
  name    = local.product_record
  type    = "A"
  value   = azurerm_public_ip.control.ip_address
  ttl     = 60
  proxied = false
}

resource "cloudflare_record" "headscale" {
  zone_id = var.cloudflare_zone_id
  name    = local.headscale_record
  type    = "A"
  value   = azurerm_public_ip.control.ip_address
  ttl     = 60
  proxied = false
}
