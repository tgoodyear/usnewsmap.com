// Public basemap tiles (PMTiles read with HTTP range requests): anonymous read
// of non-sensitive map data only, no keys (08 §8.1).

param location string
param tags object
param name string
param allowedOrigins array

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: name
  location: location
  tags: tags
  kind: 'StorageV2'
  sku: { name: 'Standard_LRS' }
  properties: {
    accessTier: 'Hot'
    allowSharedKeyAccess: false
    defaultToOAuthAuthentication: true
    allowBlobPublicAccess: true
    allowCrossTenantReplication: false
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    publicNetworkAccess: 'Enabled'
  }
}

resource blobService 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: account
  name: 'default'
  properties: {
    cors: {
      corsRules: [
        {
          allowedOrigins: allowedOrigins
          allowedMethods: ['GET', 'HEAD']
          allowedHeaders: ['range', 'if-match', 'if-none-match']
          exposedHeaders: ['content-range', 'accept-ranges', 'etag', 'content-length']
          maxAgeInSeconds: 86400
        }
      ]
    }
  }
}

resource tiles 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobService
  name: 'tiles'
  properties: { publicAccess: 'Blob' }
}

output name string = account.name
output tilesUrl string = '${account.properties.primaryEndpoints.blob}tiles'
