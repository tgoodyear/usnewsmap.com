// Data lake account (08 §8.1): flat namespace so versioning and soft delete
// work, Entra ID only (no keys or SAS), private endpoint only in steady state.

param location string
param tags object
param name string

@description('Blob containers. Access is granted per container in rbac.bicep.')
var containers = ['curated', 'reference', 'cache', 'qw-index', 'searches']

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
// searches/days/ and searches/import/ (the search log, kept indefinitely,
// ADR-0012) match no rule here, so nothing expires them.
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
        {
          // The search log's staged batches (one blob per replica per flush)
          // are timed by their creation. The API copies each day, shuffled,
          // to searches/days/ an hour after it ends; these go a week after
          // they're written (STAGING_DAYS in crates/usnm-api/src/searchlog.rs),
          // then stay in soft delete for 14 days (ADR-0012 counts that).
          name: 'expire-search-staging'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: { blobTypes: ['blockBlob'], prefixMatch: ['searches/staging/'] }
            actions: {
              baseBlob: { delete: { daysAfterModificationGreaterThan: 7 } }
              version: { delete: { daysAfterCreationGreaterThan: 1 } }
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
