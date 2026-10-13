// The BOINC group's own registry for the server and bridge images (ADR-0019, decision 6).
// Entra only, as infra/modules/registry.bicep: no admin user, no anonymous pull (Basic cannot
// turn it on). Images are built in Azure with ACR Tasks (scripts/build-boinc-images.sh), so
// the registry keeps its public endpoint, which still needs a token; private endpoints need
// Premium. The worker image that volunteers pull is not here: it is a public GHCR package
// (ADR-0019, addendum).
//
// It lives in this group, not in the app's registry in the main group, so the BOINC stack
// writes no role assignment outside its own group.
param registryName string
param location string
@description('Principals given AcrPull: the app\'s and the secrets job\'s identities.')
param pullPrincipalIds array

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' = {
  name: registryName
  location: location
  sku: { name: 'Basic' }
  properties: {
    adminUserEnabled: false
    publicNetworkAccess: 'Enabled'
  }
}

// https://learn.microsoft.com/en-us/azure/role-based-access-control/built-in-roles/containers#acrpull
var acrPull = '7f951dda-4ed3-4680-a7ca-43fe172d538d'

resource pulls 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for p in pullPrincipalIds: {
    scope: registry
    name: guid(registry.id, p, acrPull)
    properties: {
      principalId: p
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPull)
    }
  }
]

output name string = registry.name
output loginServer string = registry.properties.loginServer
