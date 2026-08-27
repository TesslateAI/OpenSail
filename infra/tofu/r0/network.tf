resource "azurerm_resource_group" "r0" {
  name     = "${var.name_prefix}-rg"
  location = var.location
}

resource "azurerm_virtual_network" "r0" {
  name                = "${var.name_prefix}-vnet"
  location            = azurerm_resource_group.r0.location
  resource_group_name = azurerm_resource_group.r0.name
  address_space       = [var.vnet_cidr]
}

resource "azurerm_network_security_group" "control" {
  name                = "${var.name_prefix}-control-nsg"
  location            = azurerm_resource_group.r0.location
  resource_group_name = azurerm_resource_group.r0.name

  security_rule {
    name                       = "https-product"
    priority                   = 100
    direction                  = "Inbound"
    access                     = "Allow"
    protocol                   = "Tcp"
    source_port_range          = "*"
    destination_port_range     = "443"
    source_address_prefix      = "Internet"
    destination_address_prefix = "*"
  }

  security_rule {
    name                       = "headscale-wireguard"
    priority                   = 110
    direction                  = "Inbound"
    access                     = "Allow"
    protocol                   = "Udp"
    source_port_range          = "*"
    destination_port_range     = "41641"
    source_address_prefix      = "Internet"
    destination_address_prefix = "*"
  }

  security_rule {
    name                       = "headscale-stun"
    priority                   = 120
    direction                  = "Inbound"
    access                     = "Allow"
    protocol                   = "Udp"
    source_port_range          = "*"
    destination_port_range     = "3478"
    source_address_prefix      = "Internet"
    destination_address_prefix = "*"
  }

  dynamic "security_rule" {
    for_each = var.management_cidrs
    content {
      name                       = "ssh-bootstrap-${security_rule.key}"
      priority                   = 200 + security_rule.key
      direction                  = "Inbound"
      access                     = "Allow"
      protocol                   = "Tcp"
      source_port_range          = "*"
      destination_port_range     = "22"
      source_address_prefix      = security_rule.value
      destination_address_prefix = "*"
    }
  }
}

resource "azurerm_subnet" "control" {
  name                 = "control"
  resource_group_name  = azurerm_resource_group.r0.name
  virtual_network_name = azurerm_virtual_network.r0.name
  address_prefixes     = [var.control_subnet_cidr]
  service_endpoints    = ["Microsoft.Storage", "Microsoft.KeyVault"]
}

resource "azurerm_subnet" "data" {
  name                 = "data"
  resource_group_name  = azurerm_resource_group.r0.name
  virtual_network_name = azurerm_virtual_network.r0.name
  address_prefixes     = [var.data_subnet_cidr]

  delegation {
    name = "postgres"
    service_delegation {
      name = "Microsoft.DBforPostgreSQL/flexibleServers"
      actions = [
        "Microsoft.Network/virtualNetworks/subnets/join/action",
      ]
    }
  }
}

resource "azurerm_subnet_network_security_group_association" "control" {
  subnet_id                 = azurerm_subnet.control.id
  network_security_group_id = azurerm_network_security_group.control.id
}

resource "azurerm_public_ip" "control" {
  name                = "${var.name_prefix}-control-ip"
  location            = azurerm_resource_group.r0.location
  resource_group_name = azurerm_resource_group.r0.name
  allocation_method   = "Static"
  sku                 = "Standard"
}

resource "azurerm_network_interface" "control" {
  name                           = "${var.name_prefix}-control-nic"
  location                       = azurerm_resource_group.r0.location
  resource_group_name            = azurerm_resource_group.r0.name
  accelerated_networking_enabled = false

  ip_configuration {
    name                          = "primary"
    subnet_id                     = azurerm_subnet.control.id
    private_ip_address_allocation = "Dynamic"
    public_ip_address_id          = azurerm_public_ip.control.id
  }
}

resource "azurerm_network_interface_security_group_association" "control" {
  network_interface_id      = azurerm_network_interface.control.id
  network_security_group_id = azurerm_network_security_group.control.id
}
