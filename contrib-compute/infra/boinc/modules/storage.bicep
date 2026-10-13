// One storage account: the BOINC project directory (upload/, download/, results/, keys/,
// apps/) and the hand-off folders as an Azure Files share, mounted into the Container App
// and the bridge job. Container Apps can only mount Azure Files with the account key, so
// shared key access has to stay ON here. This is a known gap, listed in docs/design.md.
// The account holds scans the cache already holds, volunteer results and spool files;
// no search text, no keys. Scans are no longer copied to a second account: usnm-boinc
// submit stages each cohort's cached pages into download/ through create_work.
param location string
param filesAccountName string
@description('Quota of the project share in GiB.')
param shareQuotaGb int = 200

resource files 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: filesAccountName
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

output filesAccountName string = files.name
output shareName string = share.name
