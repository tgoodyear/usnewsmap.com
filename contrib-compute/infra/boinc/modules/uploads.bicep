// Signed app versions, from an operator without VNet access to the project share
// (docs/boinc.md, "Install a signed app version"). The operator uploads a version's files and
// their .sig files to the container app-versions with Entra ID; the bridge job's
// install-versions step reads them with the server's identity, refuses any file without a
// valid signature, and places them under <project>/apps/.
//
// The project's other accounts are private-endpoint only. This one has public network access,
// because the operator's machine is outside the VNet, and nothing else open: no shared key, no
// anonymous access, TLS 1.2. The operators get Storage Blob Data Contributor on the container
// only; the server's identity gets Storage Blob Data Reader on it. It holds only code that is
// published to volunteers anyway, and is useless without the offline key's signatures.
// The public-network policy lets it through by resource id: infra/main.bicep lists
// boincUploadsAccountName() for the BOINC group's assignment.
param accountName string
param location string
@description('Entra object ids (people or groups) that upload app versions.')
param operatorPrincipalIds array
@description('The identity the bridge job runs as; it reads the uploads.')
param readerPrincipalId string
param logAnalyticsWorkspaceId string

var containerName = 'app-versions'
// https://learn.microsoft.com/en-us/azure/role-based-access-control/built-in-roles/storage
var blobDataContributor = 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
var blobDataReader = '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: accountName
  location: location
  kind: 'StorageV2'
  sku: { name: 'Standard_LRS' }
  properties: {
    accessTier: 'Hot'
    allowBlobPublicAccess: false
    allowSharedKeyAccess: false
    allowCrossTenantReplication: false
    defaultToOAuthAuthentication: true
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    publicNetworkAccess: 'Enabled'
    networkAcls: { defaultAction: 'Allow', bypass: 'None' }
  }
}

resource blobService 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: account
  name: 'default'
  properties: {
    deleteRetentionPolicy: { enabled: true, days: 7 }
    containerDeleteRetentionPolicy: { enabled: true, days: 7 }
  }
}

resource container 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobService
  name: containerName
  properties: { publicAccess: 'None' }
}

resource uploaders 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for operator in operatorPrincipalIds: {
    scope: container
    name: guid(container.id, operator, blobDataContributor)
    properties: {
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobDataContributor)
      principalId: operator
    }
  }
]

resource reader 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: container
  name: guid(container.id, readerPrincipalId, blobDataReader)
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobDataReader)
    principalId: readerPrincipalId
    principalType: 'ServicePrincipal'
  }
}

// Who uploaded or read what, and when.
resource diag 'Microsoft.Insights/diagnosticSettings@2021-05-01-preview' = {
  scope: blobService
  name: 'to-log'
  properties: {
    workspaceId: logAnalyticsWorkspaceId
    logs: [ { categoryGroup: 'audit', enabled: true } ]
  }
}

output name string = account.name
output containerName string = container.name
