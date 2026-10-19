// One storage account: the BOINC project directory (upload/, download/, results/, keys/,
// apps/) and the hand-off folders as an Azure Files share, mounted into the Container App
// and the bridge job. Container Apps can only mount Azure Files with the account key, so
// shared key access has to stay ON here. This is a known gap, listed in docs/design.md.
// The account is not public: public network access is off and the only way in is a private
// endpoint in the project's VNet (the repository's public-access guardrail would deny it otherwise).
// The account holds scans the cache already holds, volunteer results and spool files;
// no search text, no keys. Scans are no longer copied to a second account: usnm-boinc
// submit stages each cohort's cached pages into download/ through create_work.
param endpointName string
param location string
param peSubnetId string
param dnsZoneId string
param filesAccountName string
@description('Quota of the project share in GiB.')
param shareQuotaGb int = 200
@description('Keep the pay-as-you-go account (StorageV2, TransactionOptimized): every stage but provisioned-final.')
param legacyShare bool = true
@description('Create the provisioned v2 SSD account and share (projectShare copy and later).')
param provisionedShare bool = false
@description('The provisioned v2 account\'s name and its file private endpoint\'s.')
param provisionedAccountName string = ''
param provisionedEndpointName string = ''
@description('Principal that fills the provisioned share (the share copy job\'s identity): Storage File Data Privileged Contributor on that account. Empty: none.')
param copyWriterPrincipalId string = ''

resource files 'Microsoft.Storage/storageAccounts@2023-05-01' = if (legacyShare) {
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
    publicNetworkAccess: 'Disabled'
    networkAcls: { defaultAction: 'Deny', bypass: 'None' }
  }
}

resource endpoint 'Microsoft.Network/privateEndpoints@2024-01-01' = if (legacyShare) {
  name: endpointName
  location: location
  properties: {
    subnet: { id: peSubnetId }
    privateLinkServiceConnections: [
      { name: 'file', properties: { privateLinkServiceId: files.id, groupIds: [ 'file' ] } }
    ]
  }
}

resource endpointDns 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-01-01' = if (legacyShare) {
  parent: endpoint
  name: 'default'
  properties: { privateDnsZoneConfigs: [ { name: 'file', properties: { privateDnsZoneId: dnsZoneId } } ] }
}

resource fileService 'Microsoft.Storage/storageAccounts/fileServices@2023-05-01' = if (legacyShare) {
  parent: files
  name: 'default'
}

resource share 'Microsoft.Storage/storageAccounts/fileServices/shares@2023-05-01' = if (legacyShare) {
  parent: fileService
  name: 'project'
  properties: {
    shareQuota: shareQuotaGb
    enabledProtocols: 'SMB'
    accessTier: 'TransactionOptimized'
  }
}

// The provisioned v2 SSD account (#917, #529): billed by the GiB provisioned (about $0.10/GiB
// a month) with baseline IOPS (3,000 + 1 per GiB) and throughput included, and no charge per
// operation. The pay-as-you-go share cost about $21 in operations in four days (51M, 83% of
// them open, close and stat), against $0.61 of capacity. SMB, for the same Container Apps
// mount; shared key stays on for that mount, as on the old account (docs/design.md).
resource v2 'Microsoft.Storage/storageAccounts@2025-01-01' = if (provisionedShare) {
  name: provisionedAccountName
  location: location
  kind: 'FileStorage'
  sku: { name: 'PremiumV2_LRS' }
  properties: {
    allowBlobPublicAccess: false
    allowSharedKeyAccess: true // required by the Container Apps Azure Files mount
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    publicNetworkAccess: 'Disabled'
    networkAcls: { defaultAction: 'Deny', bypass: 'None' }
  }
}

resource v2Endpoint 'Microsoft.Network/privateEndpoints@2024-01-01' = if (provisionedShare) {
  name: provisionedEndpointName
  location: location
  properties: {
    subnet: { id: peSubnetId }
    privateLinkServiceConnections: [
      { name: 'file', properties: { privateLinkServiceId: v2.id, groupIds: [ 'file' ] } }
    ]
  }
}

resource v2EndpointDns 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-01-01' = if (provisionedShare) {
  parent: v2Endpoint
  name: 'default'
  properties: { privateDnsZoneConfigs: [ { name: 'file', properties: { privateDnsZoneId: dnsZoneId } } ] }
}

resource v2FileService 'Microsoft.Storage/storageAccounts/fileServices@2025-01-01' = if (provisionedShare) {
  parent: v2
  name: 'default'
}

// Provisioned size is the quota; baseline IOPS and throughput come with it.
resource v2Share 'Microsoft.Storage/storageAccounts/fileServices/shares@2025-01-01' = if (provisionedShare) {
  parent: v2FileService
  name: 'project'
  properties: {
    shareQuota: shareQuotaGb
    enabledProtocols: 'SMB'
  }
}

// Files over REST with an Entra identity takes the privileged data roles (they bypass the
// share's file ACLs, so they're scoped to the one account and only for the copy).
resource copyWriter 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (provisionedShare && !empty(copyWriterPrincipalId)) {
  scope: v2
  name: guid(v2.id, copyWriterPrincipalId, 'storage-file-data-privileged-contributor')
  properties: {
    principalId: copyWriterPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '69566ab7-960f-475b-8e7c-b3118f30c6bd')
    description: 'The BOINC share copy job (#917): writes the provisioned project share during the move.'
  }
}

output filesAccountName string = legacyShare ? files.name : ''
output shareName string = 'project'
output provisionedAccountName string = provisionedShare ? v2.name : ''
