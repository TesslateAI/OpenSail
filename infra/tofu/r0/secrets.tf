# Protected service credentials originate in Key Vault. OpenTofu generates or
# reads each value from its owning resource and writes it here; Ansible is the
# only delivery path to the control VM (client-credentials token on the
# controller, root:voie-cloud 0640 file on the host). No other transport may
# carry these values.

locals {
  # Single source for the DSN consumed by the Key Vault secret. The generated
  # password is alphanumeric, so it needs
  # no URL encoding inside the userinfo component.
  postgres_dsn = format(
    "postgres://voie:%s@%s:5432/voie?sslmode=require",
    random_password.postgres.result,
    azurerm_postgresql_flexible_server.control.fqdn,
  )
}

resource "azurerm_key_vault_secret" "postgres_password" {
  name         = "voie-postgres-password"
  value        = random_password.postgres.result
  key_vault_id = azurerm_key_vault.r0.id

  depends_on = [azurerm_key_vault_access_policy.deployer]
}

resource "azurerm_key_vault_secret" "postgres_dsn" {
  name         = "voie-postgres-dsn"
  value        = local.postgres_dsn
  key_vault_id = azurerm_key_vault.r0.id

  depends_on = [azurerm_key_vault_access_policy.deployer]
}

resource "azurerm_key_vault_secret" "blob_account_key" {
  name         = "voie-blob-account-key"
  value        = azurerm_storage_account.blob.primary_access_key
  key_vault_id = azurerm_key_vault.r0.id

  depends_on = [azurerm_key_vault_access_policy.deployer]
}

# The bootstrap native admin password follows the same rule: OpenTofu
# generates it once and Key Vault is its sole authority. The control seeds
# the admin at first boot from the delivered file and ignores it afterwards,
# so the value stays stable across applies (random_password persists in
# state) and rotation is a deliberate resource replacement.
# The bootstrap native admin password is operator-supplied through the
# deployment env (VOIE_BOOTSTRAP_ADMIN_PASSWORD) and delivered to the
# control as a 0600 file by Ansible. Terraform no longer generates or
# mirrors it: the deploy identity has Set-only Key Vault access at best,
# and the value must be known to the operator running the native proof.
variable "bootstrap_admin_password" {
  type        = string
  description = "Bootstrap native admin password delivered to the control as a 0600 file. Supplied by the deployment env; never written to Key Vault."
  default     = null
  sensitive   = true
}
