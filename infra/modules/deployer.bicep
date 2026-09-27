// What CI may do in an environment (08 §8.2): roll the API app onto a new
// image, and read the Static Web App's deployment token. One custom role,
// assigned on the environment's resource group:
//
// - Azure rejects role assignments scoped to a Static Web App or a Container
//   App itself (RoleDefinitionDoesNotExist, even for built-in Contributor).
// - An app-scoped role wouldn't be enough anyway: `az containerapp update`
//   sends the app's environment and user-assigned identity back, so Azure
//   also checks join and assign rights on those.
//
// The group holds only this environment's resources. The role can't touch
// data (storage, Cosmos), keys, networking or role assignments.

@description('The CI identity.')
param principalId string

// The name predates the Container Apps rights; it's kept so existing
// environments update the role in place.
resource role 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(resourceGroup().id, 'usnm-swa-deployer')
  properties: {
    // Role names are unique per tenant: include the subscription and group.
    roleName: 'usnm deployer (${take(subscription().subscriptionId, 8)}/${resourceGroup().name})'
    description: 'CI deploys: roll Container Apps onto new images and read Static Web App deployment tokens.'
    type: 'CustomRole'
    permissions: [
      {
        actions: [
          // az containerapp update / show, and its operation polling.
          'Microsoft.App/containerApps/read'
          'Microsoft.App/containerApps/write'
          'Microsoft.App/containerApps/listSecrets/action'
          'Microsoft.App/containerApps/revisions/read'
          'Microsoft.App/managedEnvironments/read'
          'Microsoft.App/managedEnvironments/join/action'
          'Microsoft.App/locations/*/read'
          // The app's user-assigned identity comes back in every update.
          'Microsoft.ManagedIdentity/userAssignedIdentities/read'
          'Microsoft.ManagedIdentity/userAssignedIdentities/assign/action'
          // The site's deployment token.
          'Microsoft.Web/staticSites/read'
          'Microsoft.Web/staticSites/listSecrets/action'
        ]
      }
    ]
    assignableScopes: [resourceGroup().id]
  }
}

resource assignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(resourceGroup().id, principalId, 'usnm-swa-deployer')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    // Custom roles are addressed at subscription level wherever they're defined.
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', role.name)
  }
}
