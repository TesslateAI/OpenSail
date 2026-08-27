output "resource_group_name" {
  value = azurerm_resource_group.r0.name
}

output "control_vm_name" {
  value = try(azurerm_linux_virtual_machine.control[0].name, null)
}

output "control_managed_image_id" {
  value = try(azurerm_shared_image_version.control[0].id, azurerm_image.control[0].id, null)
}

output "control_private_ip" {
  value = azurerm_network_interface.control.private_ip_address
}

output "control_public_ip" {
  value = azurerm_public_ip.control.ip_address
}

output "control_identity_client_id" {
  value = azurerm_user_assigned_identity.control.client_id
}

output "postgres_fqdn" {
  # Resolves only from inside the linked virtual network; no public answer.
  value = azurerm_postgresql_flexible_server.control.fqdn
}

output "postgres_database" {
  value = azurerm_postgresql_flexible_server_database.voie.name
}

output "postgres_user" {
  value = azurerm_postgresql_flexible_server.control.administrator_login
}

output "blob_account_name" {
  value = azurerm_storage_account.blob.name
}

output "blob_container_name" {
  value = azurerm_storage_container.dsh.name
}

output "blob_endpoint" {
  value = azurerm_storage_account.blob.primary_blob_endpoint
}

output "key_vault_name" {
  value = azurerm_key_vault.r0.name
}

output "key_vault_uri" {
  value = azurerm_key_vault.r0.vault_uri
}
output "user_secrets_key_vault_uri" {
  value = azurerm_key_vault.user_secrets.vault_uri
}

output "base_domain" {
  value = var.base_domain
}

output "public_hostname" {
  value = local.product_hostname
}

output "headscale_hostname" {
  value = local.headscale_hostname
}

output "management_cidrs" {
  value = var.management_cidrs
}

output "oidc_issuer" {
  # Consumed as VOIE_OIDC_ISSUER. Null when OIDC provisioning is disabled.
  value = try(azuread_application.control_rp[0].client_id, null) == null ? null : local.oidc_issuer
}

output "oidc_client_id" {
  # Consumed as VOIE_OIDC_CLIENT_ID. Null when OIDC provisioning is disabled.
  value = try(azuread_application.control_rp[0].client_id, null)
}

output "oidc_redirect_url" {
  # Registered web redirect URI; the product must run with the same value as
  # VOIE_OIDC_REDIRECT_URL. Null when OIDC provisioning is disabled.
  value = try(azuread_application.control_rp[0].client_id, null) == null ? null : local.oidc_redirect_url
}

output "oidc_client_secret" {
  # Deployment input only: reaches runtime as /etc/voie/secrets/oidc-client-secret.
  # Null when OIDC provisioning is disabled.
  value     = try(azuread_application_password.control_rp[0].value, null)
  sensitive = true
}
