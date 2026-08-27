# Entra ID relying-party client for the product console. The registration is
# OpenTofu-owned end to end: application, service principal, and client
# secret. The redirect URI reuses the DNS-managed public hostname, so the
# registered callback and VOIE_OIDC_REDIRECT_URL cannot diverge.
#
# Provisioning is optional and off by default: native-account estates need no
# AzureAD application, and the Graph API permission it requires must not
# block deployment. Every resource below is count-gated on var.oidc_provision;
# when disabled, no azuread app/SP/password or OIDC Key Vault secret is
# planned and the outputs resolve to null.
locals {
  # The v2 endpoint's discovery document reports exactly this issuer, which is
  # what the product validates; requesting v2 tokens keeps the issued `iss`
  # claim aligned with it.
  oidc_issuer       = "https://login.microsoftonline.com/${var.tenant_id}/v2.0"
  oidc_redirect_url = "https://${local.product_hostname}/oidc/callback"
}

resource "azuread_application" "control_rp" {
  count = var.oidc_provision ? 1 : 0

  display_name            = "${var.name_prefix}-control-rp"
  prevent_duplicate_names = true
  sign_in_audience        = "AzureADMyOrg"

  api {
    requested_access_token_version = 2
  }

  # Self-ownership keeps later applies and teardown workable under the
  # least-privilege Graph grant (Application.ReadWrite.OwnedBy).
  owners = [data.azurerm_client_config.current.object_id]

  web {
    redirect_uris = [local.oidc_redirect_url]
  }
}

# Materializes the enterprise-application object in the tenant; a bare
# registration is not enough for tenant-scoped sign-in plumbing.
resource "azuread_service_principal" "control_rp" {
  count = var.oidc_provision ? 1 : 0

  client_id                    = azuread_application.control_rp[0].client_id
  app_role_assignment_required = false
  owners                       = [data.azurerm_client_config.current.object_id]
}

# Confidential-client credential. No expiry date: rotation replaces this
# resource deliberately, and the value leaves OpenTofu state only through the
# sensitive output.
resource "azuread_application_password" "control_rp" {
  count = var.oidc_provision ? 1 : 0

  application_id = azuread_application.control_rp[0].id
  display_name   = "${var.name_prefix}-control-rp"
}

# The vault mirrors the live secret so Key Vault remains the sole credential
# authority, matching the fabric CA provenance rule. Depends on the deployer
# policy because Set is a data-plane grant the secret creation needs.
resource "azurerm_key_vault_secret" "oidc_client_secret" {
  count = var.oidc_provision ? 1 : 0

  name         = "voie-oidc-client-secret"
  value        = azuread_application_password.control_rp[0].value
  key_vault_id = azurerm_key_vault.r0.id

  depends_on = [azurerm_key_vault_access_policy.deployer]
}
