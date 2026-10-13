// Phase A: the contributor's Key Vault and the read-only identity the workers
// use. No container runs and nothing is sent to the maintainer. Afterwards the
// contributor stores the BOINC account key the maintainer gave them:
//
//   deploy-azure.sh --phase set-key --subscription ID --vault-name NAME --account-key-file FILE
//
// Then deploy vmss-spot.bicep or aci.bicep (phase B).

param location string = resourceGroup().location

@description('Globally unique Key Vault name, 3 to 24 characters.')
@minLength(3)
@maxLength(24)
param vaultName string = 'usnmc-${uniqueString(resourceGroup().id)}'

@description('Your own object id (az ad signed-in-user show --query id --output tsv). Lets you store the key. Empty skips.')
param keyWriterPrincipalId string = ''

@description('Monthly budget alert amount. 0 skips the budget.')
param budgetAmount int = 0

param budgetContactEmails array = []

var workerIdentityName = 'usnm-worker-id'

module vault 'modules/vault.bicep' = {
  name: 'vault'
  params: {
    keyWriterPrincipalId: keyWriterPrincipalId
    location: location
    vaultName: vaultName
    workerIdentityName: workerIdentityName
  }
}

module budget 'modules/budget.bicep' = if (budgetAmount > 0) {
  name: 'budget'
  params: {
    amount: budgetAmount
    contactEmails: budgetContactEmails
  }
}

output vaultName string = vault.outputs.vaultName
output workerIdentityName string = workerIdentityName
