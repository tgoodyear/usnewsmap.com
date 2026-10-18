// The on-prem BOINC batch controller's role on one group (boinc-controller.bicep).

param principalId string
@description('The controller role\'s resource id.')
param roleId string

resource grant 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(resourceGroup().id, principalId, 'boinc-controller')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: roleId
  }
}
