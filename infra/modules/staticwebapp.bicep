// SPA hosting (08 §8.1, ADR-0006): Free tier replaces Front Door for the site.
// Content is deployed by CI with the SWA deployment token; no repo link here.

param location string
param tags object
param name string
@allowed(['Free', 'Standard'])
param sku string = 'Free'
@description('Principal allowed to deploy the site (the CI identity); empty for none.')
param deployerPrincipalId string = ''

resource swa 'Microsoft.Web/staticSites@2023-12-01' = {
  name: name
  location: location
  tags: tags
  sku: { name: sku, tier: sku }
  properties: {
    stagingEnvironmentPolicy: 'Enabled'
    allowConfigFileUpdates: true
  }
}

// CI reads the deployment token at deploy time (no stored secret):
// Contributor scoped to this one site.
var contributor = 'b24988ac-6180-42a3-ab7e-976ab8b6e2d0'

resource deployer 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(deployerPrincipalId)) {
  scope: swa
  name: guid(swa.id, deployerPrincipalId, contributor)
  properties: {
    principalId: deployerPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', contributor)
  }
}

output id string = swa.id
output name string = swa.name
output defaultHostname string = swa.properties.defaultHostname
