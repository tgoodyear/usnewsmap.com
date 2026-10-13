// Key Vault for the BOINC project secrets, read by the app's managed identity.
// Secrets are created by the operator bootstrap script, not by this template, so
// no secret value passes through a deployment parameter or a template output.
// Expected secret names: mysql-admin-password, mysql-runtime-password,
// upload-private-key, upload-public-key. The code-signing private key is NOT here.
param name string
param location string
param readerPrincipalId string
param logAnalyticsWorkspaceId string

var vaultName = take('usnm${name}kv${uniqueString(resourceGroup().id, name)}', 24)

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' = {
  name: vaultName
  location: location
  properties: {
    tenantId: subscription().tenantId
    sku: { family: 'A', name: 'standard' }
    enableRbacAuthorization: true
    enableSoftDelete: true
    softDeleteRetentionInDays: 30
    enablePurgeProtection: true
  }
}

// Key Vault Secrets User
var secretsUser = '4633458b-17de-408a-b874-0445c86b69e6'

resource readerRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, readerPrincipalId, secretsUser)
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', secretsUser)
    principalId: readerPrincipalId
    principalType: 'ServicePrincipal'
  }
}

resource diag 'Microsoft.Insights/diagnosticSettings@2021-05-01-preview' = {
  scope: vault
  name: 'to-log'
  properties: {
    workspaceId: logAnalyticsWorkspaceId
    logs: [ { categoryGroup: 'audit', enabled: true } ]
  }
}

output name string = vault.name
output uri string = vault.properties.vaultUri
