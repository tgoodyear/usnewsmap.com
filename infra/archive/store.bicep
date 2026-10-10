// The archival account (infra/archive/main.bicep): Entra only (no
// keys, no SAS), no public network access (private endpoints from the
// environments that use it), versioning and 14-day soft delete, archives at
// the Cold tier, and a delete lock.

param location string
param tags object
param name string

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

// Batch archives as LoC served them, with a manifest each (crates/usnm-ingest/src/raw.rs).
resource raw 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobService
  name: 'raw'
  properties: { publicAccess: 'None' }
}

// Packaged sample sets, immutable once written (docs/operations.md, "Sample sets").
resource sets 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobService
  name: 'sets'
  properties: { publicAccess: 'None' }
}

// Curation writes archives at Cold already; this moves anything written
// at another tier there, and prunes old versions of the small manifests.
resource lifecycle 'Microsoft.Storage/storageAccounts/managementPolicies@2023-05-01' = {
  parent: account
  name: 'default'
  properties: {
    policy: {
      rules: [
        {
          name: 'archive-to-cold'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: { blobTypes: ['blockBlob'], prefixMatch: ['raw/', 'sets/'] }
            actions: {
              baseBlob: { tierToCold: { daysAfterModificationGreaterThan: 0 } }
              version: { delete: { daysAfterCreationGreaterThan: 30 } }
            }
          }
        }
      ]
    }
  }
}

// Nobody deletes the account by accident. A lock on a private endpoint's
// target can block removing the endpoint, so scripts/archive-store.sh lifts
// it for that and puts it back (docs/operations.md, "Archival storage").
resource lock 'Microsoft.Authorization/locks@2020-05-01' = {
  scope: account
  name: 'usnm-archive-keep'
  properties: {
    level: 'CanNotDelete'
    notes: 'The archival account outlives every environment. scripts/archive-store.sh lifts this to remove an environment\'s private endpoint.'
  }
}

output id string = account.id
output name string = account.name
