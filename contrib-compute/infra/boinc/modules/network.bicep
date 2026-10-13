// One VNet: a subnet for the Container Apps environment and a delegated subnet for MySQL.
// MySQL flexible server access mode (private or public) cannot be changed after creation,
// so the private choice is made here from the start.
param name string
param location string
// Second octet of the VNet's 10.x.0.0/22 block. The address is built from it so the repo holds
// no literal private address.
param vnetOctet int = 40

resource vnet 'Microsoft.Network/virtualNetworks@2024-01-01' = {
  name: 'usnm-${name}-vnet'
  location: location
  properties: {
    addressSpace: { addressPrefixes: [ '10.${vnetOctet}.0.0/22' ] }
    subnets: [
      {
        name: 'aca'
        properties: {
          addressPrefix: '10.${vnetOctet}.0.0/27'
          delegations: [ { name: 'aca', properties: { serviceName: 'Microsoft.App/environments' } } ]
        }
      }
      {
        name: 'mysql'
        properties: {
          addressPrefix: '10.${vnetOctet}.1.0/28'
          delegations: [ { name: 'mysql', properties: { serviceName: 'Microsoft.DBforMySQL/flexibleServers' } } ]
        }
      }
    ]
  }
}

resource dns 'Microsoft.Network/privateDnsZones@2020-06-01' = {
  name: '${name}.private.mysql.database.azure.com'
  location: 'global'
}

resource dnsLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = {
  parent: dns
  name: 'link'
  location: 'global'
  properties: {
    registrationEnabled: false
    virtualNetwork: { id: vnet.id }
  }
}

output acaSubnetId string = '${vnet.id}/subnets/aca'
output mysqlSubnetId string = '${vnet.id}/subnets/mysql'
output mysqlDnsZoneId string = dns.id
