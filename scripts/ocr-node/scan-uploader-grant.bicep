param scope string
param principal string
param role string

var parts = split(scope, '/')
resource account 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: parts[8]
}
resource service 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' existing = {
  parent: account
  name: 'default'
}
resource scans 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' existing = {
  parent: service
  name: 'scans'
}
resource grant 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: scans
  name: guid(scans.id, principal, 'usnm-scans-writer')
  properties: {
    principalId: principal
    principalType: 'ServicePrincipal'
    roleDefinitionId: role
  }
}
