// Deployed in the archival account's group by an environment's stack: the
// environment's roles on the account's containers, each scoped to one
// container. Curation (id-usnm-ingest) writes `raw`.

param accountName string
@description('[{container, principalId, role}]: role is reader or contributor.')
param grants array

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: accountName

  resource blobs 'blobServices' existing = {
    name: 'default'
  }
}

resource containers 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' existing = [
  for g in grants: {
    parent: account::blobs
    name: g.container
  }
]

var roles = {
  reader: '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'
  contributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
}

resource assignments 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (g, i) in grants: {
    scope: containers[i]
    name: guid(containers[i].id, g.principalId, roles[g.role])
    properties: {
      principalId: g.principalId
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles[g.role])
    }
  }
]
