// VNet with a delegated Container Apps subnet and a private-endpoint subnet,
// plus the private DNS zones the endpoints register in (08 §8.1, ADR-0008).

param location string
param tags object
param name string
param addressPrefix string = '10.40.0.0/24'
param caeSubnetPrefix string = '10.40.0.0/27'
param peSubnetPrefix string = '10.40.0.32/28'

var dnsZones = [
  'privatelink.blob.${environment().suffixes.storage}'
  'privatelink.documents.azure.com'
]

resource vnet 'Microsoft.Network/virtualNetworks@2024-05-01' = {
  name: name
  location: location
  tags: tags
  properties: {
    addressSpace: { addressPrefixes: [addressPrefix] }
    subnets: [
      {
        name: 'snet-cae'
        properties: {
          addressPrefix: caeSubnetPrefix
          delegations: [
            {
              name: 'containerapps'
              properties: { serviceName: 'Microsoft.App/environments' }
            }
          ]
        }
      }
      {
        name: 'snet-pe'
        properties: {
          addressPrefix: peSubnetPrefix
          privateEndpointNetworkPolicies: 'Disabled'
        }
      }
    ]
  }
}

resource zones 'Microsoft.Network/privateDnsZones@2024-06-01' = [
  for zone in dnsZones: {
    name: zone
    location: 'global'
    tags: tags
  }
]

resource links 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2024-06-01' = [
  for (zone, i) in dnsZones: {
    parent: zones[i]
    name: '${name}-link'
    location: 'global'
    tags: tags
    properties: {
      registrationEnabled: false
      virtualNetwork: { id: vnet.id }
    }
  }
]

output vnetId string = vnet.id
output vnetName string = vnet.name
output caeSubnetId string = '${vnet.id}/subnets/snet-cae'
output peSubnetId string = '${vnet.id}/subnets/snet-pe'
output blobDnsZoneId string = zones[0].id
output cosmosDnsZoneId string = zones[1].id
