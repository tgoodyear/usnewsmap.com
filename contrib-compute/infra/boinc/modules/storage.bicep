// Two storage accounts, kept apart on purpose.
//
// staging: public-domain scans only, served to clients by remote-file URL with a
//   short-lived user-delegation SAS. No shared keys, no public blob access.
//   A lifecycle rule deletes blobs after 14 days.
// files: the BOINC project directory (upload/, download/, keys/, apps/) as an Azure
//   Files share mounted into the Container App. Container Apps can only mount Azure
//   Files with the account key, so shared key access has to stay ON here. This is a
//   known gap, listed in docs/design.md. The account holds results, not scans.
param name string
param location string
@description('Principal id of the app identity; it writes staged scans and mints delegation SAS.')
param appPrincipalId string
@description('Quota of the project share in GiB.')
param shareQuotaGb int = 200
@description('Days before staged scans are deleted.')
param stagingRetentionDays int = 14

var suffix = uniqueString(resourceGroup().id, name)

resource staging 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('usnm${name}stg${suffix}', 24)
  location: location
  kind: 'StorageV2'
  sku: { name: 'Standard_LRS' }
  properties: {
    allowBlobPublicAccess: false
    allowSharedKeyAccess: false
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    accessTier: 'Hot'
  }
}

resource stagingBlob 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: staging
  name: 'default'
}

resource scans 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: stagingBlob
  name: 'scans'
  properties: { publicAccess: 'None' }
}

resource lifecycle 'Microsoft.Storage/storageAccounts/managementPolicies@2023-05-01' = {
  parent: staging
  name: 'default'
  properties: {
    policy: {
      rules: [
        {
          name: 'expire-staged-scans'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: { blobTypes: [ 'blockBlob' ], prefixMatch: [ 'scans/' ] }
            actions: { baseBlob: { delete: { daysAfterModificationGreaterThan: stagingRetentionDays } } }
          }
        }
      ]
    }
  }
}

var blobContributor = 'ba92f5b4-2d11-453d-a403-e96b0029c9fe' // Storage Blob Data Contributor
var blobDelegator = 'db58b8e5-c6ad-4a2a-8342-4190687cbf4a' // Storage Blob Delegator

resource contributorRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: staging
  name: guid(staging.id, appPrincipalId, blobContributor)
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobContributor)
    principalId: appPrincipalId
    principalType: 'ServicePrincipal'
  }
}

resource delegatorRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: staging
  name: guid(staging.id, appPrincipalId, blobDelegator)
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobDelegator)
    principalId: appPrincipalId
    principalType: 'ServicePrincipal'
  }
}

resource files 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('usnm${name}fil${suffix}', 24)
  location: location
  kind: 'StorageV2'
  sku: { name: 'Standard_LRS' }
  properties: {
    allowBlobPublicAccess: false
    allowSharedKeyAccess: true // required by the Container Apps Azure Files mount
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    accessTier: 'Hot'
  }
}

resource fileService 'Microsoft.Storage/storageAccounts/fileServices@2023-05-01' = {
  parent: files
  name: 'default'
}

resource share 'Microsoft.Storage/storageAccounts/fileServices/shares@2023-05-01' = {
  parent: fileService
  name: 'project'
  properties: {
    shareQuota: shareQuotaGb
    enabledProtocols: 'SMB'
    accessTier: 'TransactionOptimized'
  }
}

output stagingAccountName string = staging.name
output stagingBlobEndpoint string = staging.properties.primaryEndpoints.blob
output filesAccountName string = files.name
output shareName string = share.name
