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

// CI reads the deployment token at deploy time (no stored secret). Azure
// rejects role assignments scoped to a Static Web App itself
// (RoleDefinitionDoesNotExist, even for built-in roles), so a custom role
// that can only read static sites and list their secrets is assigned on this
// resource group, which holds only this one site.
resource deployRole 'Microsoft.Authorization/roleDefinitions@2022-04-01' = if (!empty(deployerPrincipalId)) {
  name: guid(resourceGroup().id, 'usnm-swa-deployer')
  properties: {
    // Role names are unique per tenant: include the subscription and group.
    roleName: 'usnm static web app deployer (${take(subscription().subscriptionId, 8)}/${resourceGroup().name})'
    description: 'Read Static Web Apps and their deployment token, for CI deploys.'
    type: 'CustomRole'
    permissions: [
      {
        actions: [
          'Microsoft.Web/staticSites/read'
          'Microsoft.Web/staticSites/listSecrets/action'
        ]
      }
    ]
    assignableScopes: [resourceGroup().id]
  }
}

resource deployer 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(deployerPrincipalId)) {
  name: guid(resourceGroup().id, deployerPrincipalId, 'usnm-swa-deployer')
  properties: {
    principalId: deployerPrincipalId
    principalType: 'ServicePrincipal'
    // Custom roles are addressed at subscription level wherever they're defined.
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', deployRole!.name)
  }
}

output id string = swa.id
output name string = swa.name
output defaultHostname string = swa.properties.defaultHostname
