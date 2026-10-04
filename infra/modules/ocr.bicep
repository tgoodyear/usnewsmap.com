// Azure AI Document Intelligence for OCR of the Japanese pages LoC has no
// text for (#128). Entra ID only: local (key) auth is off, and callers need
// Cognitive Services User. Public network access stays on while the engine
// comparison runs from an operator's machine; a bulk run from a job would
// move behind a private endpoint like Cosmos and Blob.

param location string
param tags object
param name string
@description('F0: 500 pages a month free, 4 MB per image, one F0 account per subscription. S0 is pay as you go.')
@allowed(['F0', 'S0'])
param sku string = 'F0'
param workspaceId string
@description('Entra object ids of people or groups that may call the service.')
param users array = []
@description('Managed identities (service principals) that may call the service.')
param identities array = []

// Cognitive Services User: call the data-plane APIs, no keys or management.
var cognitiveServicesUser = 'a97b65f3-24c7-4388-baec-2e87135dc908'

resource account 'Microsoft.CognitiveServices/accounts@2024-10-01' = {
  name: name
  location: location
  tags: tags
  kind: 'FormRecognizer'
  sku: { name: sku }
  identity: { type: 'SystemAssigned' }
  properties: {
    customSubDomainName: name
    disableLocalAuth: true
    publicNetworkAccess: 'Enabled'
    networkAcls: { defaultAction: 'Allow' }
  }
}

resource access 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for user in users: {
    scope: account
    name: guid(account.id, user, cognitiveServicesUser)
    properties: {
      principalId: user
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', cognitiveServicesUser)
    }
  }
]

// principalType set, so a just-created identity isn't looked up before it
// has propagated (PrincipalNotFound).
resource identityAccess 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for id in identities: {
    scope: account
    name: guid(account.id, id, cognitiveServicesUser)
    properties: {
      principalId: id
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', cognitiveServicesUser)
    }
  }
]

// Audit only: RequestResponse would log every page sent.
resource logs 'Microsoft.Insights/diagnosticSettings@2021-05-01-preview' = {
  scope: account
  name: 'to-log-analytics'
  properties: {
    workspaceId: workspaceId
    logs: [{ category: 'Audit', enabled: true }]
  }
}

output endpoint string = account.properties.endpoint
output name string = account.name
