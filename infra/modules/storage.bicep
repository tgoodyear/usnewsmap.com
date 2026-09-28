// Data lake account (08 §8.1): flat namespace so versioning and soft delete
// work, Entra ID only (no keys or SAS), private endpoint only in steady state.

param location string
param tags object
param name string

@description('Blob containers. Access is granted per container in rbac.bicep.')
var containers = ['curated', 'reference', 'cache', 'qw-index']

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
    isVersioningEnabled: true
    deleteRetentionPolicy: { enabled: true, days: 14 }
    containerDeleteRetentionPolicy: { enabled: true, days: 14 }
  }
}

resource blobContainers 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = [
  for c in containers: {
    parent: blobService
    name: c
    properties: { publicAccess: 'None' }
  }
]

// Versioning is account-wide; only curated/ and reference/ need history.
resource lifecycle 'Microsoft.Storage/storageAccounts/managementPolicies@2023-05-01' = {
  parent: account
  name: 'default'
  properties: {
    policy: {
      rules: [
        {
          // Cache entries are keyed by index version; versions publish at
          // most weekly, so anything older than 14 days is for a retired version.
          name: 'expire-response-cache'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: { blobTypes: ['blockBlob'], prefixMatch: ['cache/'] }
            actions: {
              baseBlob: { delete: { daysAfterModificationGreaterThan: 14 } }
              version: { delete: { daysAfterCreationGreaterThan: 1 } }
            }
          }
        }
        {
          // The Quickwit metastore rewrites small JSON files often.
          name: 'prune-index-versions'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: { blobTypes: ['blockBlob'], prefixMatch: ['qw-index/'] }
            actions: {
              version: { delete: { daysAfterCreationGreaterThan: 7 } }
            }
          }
        }
        {
          name: 'prune-data-versions'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: { blobTypes: ['blockBlob'], prefixMatch: ['curated/', 'reference/'] }
            actions: {
              version: { delete: { daysAfterCreationGreaterThan: 30 } }
            }
          }
        }
      ]
    }
  }
}

output id string = account.id
output name string = account.name
output blobEndpoint string = account.properties.primaryEndpoints.blob
