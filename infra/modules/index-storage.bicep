// Quickwit splits and metastore (#283): merges delete inputs immediately.
// No blob/container soft delete or versioning. Recovery is a retained index
// version or a rebuild, not blob history. Private endpoint only, ADR-0009.
param location string
param tags object
param name string
var containers = ['qw-index']

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: name
  location: location
  tags: tags
  kind: 'StorageV2'
  sku: { name: 'Standard_LRS' }
  properties: {
    accessTier: 'Hot'
    isHnsEnabled: false
    allowSharedKeyAccess: false
    defaultToOAuthAuthentication: true
    allowBlobPublicAccess: false
    allowCrossTenantReplication: false
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    publicNetworkAccess: 'Disabled'
    networkAcls: {
      defaultAction: 'Deny'
      bypass: 'None'
    }
  }
}

resource blobService 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: account
  name: 'default'
  properties: {
    isVersioningEnabled: false
    deleteRetentionPolicy: { enabled: false }
    containerDeleteRetentionPolicy: { enabled: false }
  }
}

resource blobContainers 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = [
  for c in containers: {
    parent: blobService
    name: c
    properties: { publicAccess: 'None' }
  }
]

output id string = account.id
output name string = account.name
