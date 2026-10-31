@description('Existing production CI vault; does not change its networking or access model.')
param vaultName string

@description('Object ID of the existing production GitHub OIDC service principal.')
param ciPrincipalId string

param secretNames array = [
  'usnm-prod-workers-ci-token'
  'usnm-prod-cloudflare-account-id'
]

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: vaultName
}

resource ciSecrets 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = [for name in secretNames: {
  parent: vault
  name: name
}]

var secretsUser = '4633458b-17de-408a-b874-0445c86b69e6'

resource readers 'Microsoft.Authorization/roleAssignments@2022-04-01' = [for (name, i) in secretNames: {
  scope: ciSecrets[i]
  name: guid(ciSecrets[i].id, ciPrincipalId, secretsUser)
  properties: {
    principalId: ciPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', secretsUser)
  }
}]
