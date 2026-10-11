// Retained batch archives (`retainRaw`, docs/operations.md "Archival
// storage"): the `raw` container, which curation writes each archive it
// downloads to (Blob access tier Cold, set per blob as it is written) and
// reads it back from instead of LoC. For benchmark sets in a dev environment;
// production keeps no archives (ADR-0006). Turning the setting off deletes
// the container and what it holds (soft delete keeps it 14 days).

@description('The data account.')
param storageAccountName string
@description('The curation identity (id-usnm-ingest): Blob Data Contributor on `raw` only.')
param ingestPrincipalId string

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageAccountName

  resource blobs 'blobServices' existing = {
    name: 'default'
  }
}

resource raw 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: account::blobs
  name: 'raw'
  properties: { publicAccess: 'None' }
}

var blobContributor = 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'

resource writer 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: raw
  name: guid(raw.id, ingestPrincipalId, blobContributor)
  properties: {
    principalId: ingestPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobContributor)
  }
}

output container string = raw.name
