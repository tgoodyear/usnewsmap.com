// An environment's access to the archival account (`USNM_ARCHIVE_ACCOUNT`,
// infra/archive/): a private endpoint in its own VNet, registered in its
// Blob private DNS zone. The roles on the account's containers are
// archive-grant.bicep, deployed in the account's group. Removing the setting
// removes these, never the account or its data.

param location string
param tags object
param name string
param subnetId string
param accountId string
param blobDnsZoneId string

resource pe 'Microsoft.Network/privateEndpoints@2024-05-01' = {
  name: name
  location: location
  tags: tags
  properties: {
    subnet: { id: subnetId }
    privateLinkServiceConnections: [
      {
        name: name
        properties: {
          privateLinkServiceId: accountId
          groupIds: ['blob']
        }
      }
    ]
  }
}

resource zoneGroup 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-05-01' = {
  parent: pe
  name: 'default'
  properties: {
    privateDnsZoneConfigs: [
      {
        name: 'blob'
        properties: { privateDnsZoneId: blobDnsZoneId }
      }
    ]
  }
}
