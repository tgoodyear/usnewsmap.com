// Private endpoints for Blob and Cosmos in snet-pe, registered in the linked
// private DNS zones (08 §8.3).

param location string
param tags object
param subnetId string
param storageId string
param cosmosId string
param blobDnsZoneId string
param cosmosDnsZoneId string

var endpoints = [
  { name: 'pe-usnm-blob', target: storageId, group: 'blob', zone: blobDnsZoneId }
  { name: 'pe-usnm-cosmos', target: cosmosId, group: 'Sql', zone: cosmosDnsZoneId }
]

resource pe 'Microsoft.Network/privateEndpoints@2024-05-01' = [
  for e in endpoints: {
    name: e.name
    location: location
    tags: tags
    properties: {
      subnet: { id: subnetId }
      privateLinkServiceConnections: [
        {
          name: e.name
          properties: {
            privateLinkServiceId: e.target
            groupIds: [e.group]
          }
        }
      ]
    }
  }
]

resource zoneGroups 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-05-01' = [
  for (e, i) in endpoints: {
    parent: pe[i]
    name: 'default'
    properties: {
      privateDnsZoneConfigs: [
        {
          name: e.group
          properties: { privateDnsZoneId: e.zone }
        }
      ]
    }
  }
]
