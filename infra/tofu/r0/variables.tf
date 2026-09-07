variable "subscription_id" {
  type        = string
  description = "Azure subscription that owns this Release 0 stack. Supplied ephemerally."
}

variable "tenant_id" {
  type        = string
  description = "Azure tenant for Key Vault. Supplied ephemerally."
}

variable "location" {
  type        = string
  description = "Azure region for the control estate. Supplied ephemerally."
}

variable "name_prefix" {
  type        = string
  description = "Short label used to name provider resources. Not a cloud identity."
  default     = "voie-r0"
}

variable "vnet_cidr" {
  type        = string
  description = "Private address space for the control virtual network."
  default     = "10.20.0.0/16"
}

variable "control_subnet_cidr" {
  type        = string
  description = "Subnet CIDR for the NixOS control VM."
  default     = "10.20.1.0/24"
}

variable "data_subnet_cidr" {
  type        = string
  description = "Delegated subnet CIDR for Azure PostgreSQL."
  default     = "10.20.2.0/24"
}

variable "control_image_id" {
  type        = string
  default     = ""
  description = "Azure image resource ID of the NixOS control generation. Empty uses the OpenTofu-managed image when the VHD is uploaded."
}

variable "control_image_vhd_path" {
  type        = string
  default     = ""
  description = "Local NixOS Azure VHD path for OpenTofu to upload. Empty skips image creation."
}

variable "control_vm_size" {
  type        = string
  description = "Azure VM SKU for the single control instance."
  default     = "Standard_D2ls_v7"
}

variable "admin_username" {
  type        = string
  description = "Automation SSH user created on the control VM."
  default     = "voie"
}

variable "admin_ssh_public_key" {
  type        = string
  description = "OpenSSH public key for the automation user. Supplied ephemerally."
}

variable "management_cidrs" {
  type        = list(string)
  description = "CIDRs allowed to reach TCP/22 for persistent operator management. Empty disables public management access; C8 never rewrites this setting."
  default     = []
}

variable "cloudflare_zone_id" {
  type        = string
  description = "Cloudflare zone that publishes the TLS endpoints. Supplied ephemerally."
}

variable "cloudflare_api_token" {
  type        = string
  sensitive   = true
  description = "Cloudflare API token with Zone DNS Edit on the product zone. Supplied ephemerally."
}

variable "base_domain" {
  type        = string
  description = "DNS suffix for this control stack. Headscale is always hs.<base_domain>."
}

variable "public_hostname" {
  type        = string
  default     = ""
  description = "Product FQDN. Empty uses base_domain."
}

variable "postgres_version" {
  type        = string
  description = "Azure PostgreSQL Flexible Server major version."
  default     = "16"
}

variable "postgres_sku" {
  type        = string
  description = "Azure PostgreSQL Flexible Server SKU."
  default     = "B_Standard_B1ms"
}

variable "postgres_storage_mb" {
  type        = number
  description = "PostgreSQL allocated storage in MB."
  default     = 32768
}
variable "oidc_provision" {
  type        = bool
  description = "Provision the AzureAD OIDC relying party (application, service principal, client secret, Key Vault mirror). Disabled by default for native-account estates; enabling requires Graph API permission on the tenant."
  default     = false
}
