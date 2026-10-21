// Exempts the BOINC Key Vault alone from the resource group's deny-public-network policy
// (usnm-audit-data-public-network) while its public endpoint admits the home network's
// addresses (keyvault.bicep allowedIps; #917, maintainer decision of 20 October 2026).
// Deployed before the vault module: the policy denies the vault's update otherwise. The vault
// must already exist (a private-only deployment comes first).
param vaultName string
param policyAssignmentId string

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: vaultName
}

resource exemption 'Microsoft.Authorization/policyExemptions@2022-07-01-preview' = {
  name: 'usnm-boinc-kv-home-ip'
  scope: vault
  properties: {
    policyAssignmentId: policyAssignmentId
    exemptionCategory: 'Mitigated'
    displayName: 'BOINC Key Vault: public endpoint behind an IP firewall (#917)'
    description: 'The vault admits only the home network address(es) on its public endpoint while BOINC moves on-prem (maintainer decision, 20 October 2026). RBAC still governs access.'
  }
}
