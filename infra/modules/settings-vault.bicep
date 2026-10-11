// The environment's settings vault (#295, ADR-0006): its private settings,
// the USNM_* inputs that infra/env/schema.json marks private (alert
// addresses, reader principals, certificate and account resource ids, the
// repository's ids). None is a credential (ADR-0009: there are none); they
// are configuration that stays out of the public repository.
// scripts/provision.sh reads them with the operator's az sign-in and passes
// them to infra/main.bicepparam through the environment, like the committed
// settings; the template never reads or writes a secret.
//
// Access is Azure RBAC only (no access policies), Entra ID only like every
// Key Vault: the account that deploys the stack gets Key Vault Secrets
// Officer (it sets them), the environment's CI identity Key Vault Secrets
// User (read). Network: a public endpoint with no firewall, like the
// registry and Log Analytics, so laptops and GitHub runners reach it; a
// private endpoint would cost about $7.30 a month and neither could. The
// public-network policy exempts it by resource id (main.bicep).
//
// Purge protection is on in prod. Elsewhere it's off and soft delete keeps
// a deleted vault for the minimum 7 days, so teardown.sh can purge it and a
// rebuilt dev environment can use the same name (ADR-0017, open question 1).

param location string
param tags object
param name string
@description('Keep deleted vaults and secrets for 90 days and refuse purges (prod). Off: 7 days, purgeable. Both are fixed when the vault is created.')
param purgeProtection bool
@description('The CI identity\'s principal id, given Key Vault Secrets User.')
param ciPrincipalId string

// https://learn.microsoft.com/en-us/azure/role-based-access-control/built-in-roles/security#key-vault-secrets-officer
var secretsOfficer = 'b86a8fe4-44ce-4948-aee5-eccb2c155cd7'
// https://learn.microsoft.com/en-us/azure/role-based-access-control/built-in-roles/security#key-vault-secrets-user
var secretsUser = '4633458b-17de-408a-b874-0445c86b69e6'

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' = {
  name: name
  location: location
  tags: tags
  properties: {
    tenantId: tenant().tenantId
    sku: { family: 'A', name: 'standard' }
    enableRbacAuthorization: true
    accessPolicies: []
    enabledForDeployment: false
    enabledForDiskEncryption: false
    enabledForTemplateDeployment: false
    enableSoftDelete: true
    softDeleteRetentionInDays: purgeProtection ? 90 : 7
    // Azure accepts only true or unset here: once on, it can't be turned off.
    enablePurgeProtection: purgeProtection ? true : null
    publicNetworkAccess: 'Enabled'
    networkAcls: {
      defaultAction: 'Allow'
      bypass: 'None'
      ipRules: []
      virtualNetworkRules: []
    }
  }
}

// The account deploying the stack: the operator, who runs provision.sh.
resource officer 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, deployer().objectId, secretsOfficer)
  properties: {
    principalId: deployer().objectId
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', secretsOfficer)
  }
}

resource ciReader 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, ciPrincipalId, secretsUser)
  properties: {
    principalId: ciPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', secretsUser)
  }
}

output name string = vault.name
