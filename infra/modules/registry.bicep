// Private container registry (08 §8.6). Images are never public: pulls need
// Entra ID (the app and ingest identities have AcrPull); there is no
// admin user and no anonymous pull. CI pushes from `main` as `id-usnm-ci`, a
// user-assigned identity that GitHub Actions signs in to with OIDC (a
// federated credential; no secret is stored anywhere).
//
// Basic tier: private endpoints need Premium (~$50/month), so the registry
// keeps its public endpoint, which still requires a token. Images are code,
// not data; the data services stay private (ADR-0008).

param location string
param tags object
param name string
param ciIdentityName string
@description('GitHub repository allowed to push, as it appears in the OIDC subject claim: `owner@ownerId/name@repoId` (GitHub\'s immutable-ID format, which this repository uses), or `owner/name` for repositories on the older format.')
param githubRepo string
param pullPrincipalIds array

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' = {
  name: name
  location: location
  tags: tags
  sku: { name: 'Basic' }
  properties: {
    // No admin user; anonymous pull is off by default (and needs Standard).
    adminUserEnabled: false
    publicNetworkAccess: 'Enabled'
  }
}

resource ci 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: ciIdentityName
  location: location
  tags: tags

  // Only workflow runs on this repository's main branch can use it.
  resource github 'federatedIdentityCredentials' = {
    name: 'github-main'
    properties: {
      issuer: 'https://token.actions.githubusercontent.com'
      subject: 'repo:${githubRepo}:ref:refs/heads/main'
      audiences: ['api://AzureADTokenExchange']
    }
  }
}

var acrPull = '7f951dda-4ed3-4680-a7ca-43fe172d538d'
var acrPush = '8311e382-0749-4cb8-b61a-304f252e45ec'

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

resource push 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: registry
  name: guid(registry.id, ci.id, acrPush)
  properties: {
    principalId: ci.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPush)
  }
}

output name string = registry.name
output loginServer string = registry.properties.loginServer
output ciClientId string = ci.properties.clientId
