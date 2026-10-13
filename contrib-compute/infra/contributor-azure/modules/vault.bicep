// A Key Vault for one contributor's BOINC account key, plus a managed identity
// that may only read it (used by the workers). RBAC mode, no access policies,
// purge protection on. The contributor stores the key once with
// `deploy-azure.sh --phase set-key`; for that, pass their own object id as
// keyWriterPrincipalId so they hold Key Vault Secrets Officer on this vault.

@description('Globally unique, 3 to 24 characters.')
@minLength(3)
@maxLength(24)
param vaultName string

param location string = resourceGroup().location

@description('Object id of the contributor (az ad signed-in-user show --query id). Gets Key Vault Secrets Officer so they can store the key. Empty skips.')
param keyWriterPrincipalId string = ''

@description('Name of the identity that reads the key (phase B workers).')
param workerIdentityName string = 'usnm-worker-id'

var secretsOfficer = 'b86a8fe4-44ce-4948-aee5-eccb2c155cd7'
var secretsUser = '4633458b-17de-408a-b874-0445c86b69e6'

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' = {
  name: vaultName
  location: location
  properties: {
    sku: {
      family: 'A'
      name: 'standard'
    }
    tenantId: subscription().tenantId
    enableRbacAuthorization: true
    enableSoftDelete: true
    softDeleteRetentionInDays: 90
    enablePurgeProtection: true
    publicNetworkAccess: 'Enabled'
  }
}

resource workerIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: workerIdentityName
  location: location
}

resource writerRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(keyWriterPrincipalId)) {
  scope: vault
  name: guid(vault.id, keyWriterPrincipalId, secretsOfficer)
  properties: {
    principalId: keyWriterPrincipalId
    principalType: 'User'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', secretsOfficer)
  }
}

resource workerRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, workerIdentity.id, secretsUser)
  properties: {
    principalId: workerIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', secretsUser)
  }
}

output vaultName string = vault.name
output workerIdentityClientId string = workerIdentity.properties.clientId
output workerIdentityId string = workerIdentity.id
