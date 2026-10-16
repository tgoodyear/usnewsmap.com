// Only the BOINC dev prerequisites; never deploy the application's full dev stack for this test.
targetScope = 'subscription'

import { usnmName, usnmCompactName } from '../../../infra/naming.bicep'
import { guardrailPolicyNames } from '../../../infra/guardrails.bicep'

@allowed(['westus2'])
param location string = 'westus2'

@description('Approved UTC teardown deadline, recorded as a tag; not an automatic deletion mechanism.')
@minLength(20)
param expiresAt string

var groupName = usnmName('boinc', 'rg', 'dev')
var registryName = usnmCompactName('boinc', 'cr', 'dev', take(uniqueString(subscription().id, 'boinc-dev'), 6))
var definitionIds = [for name in items(guardrailPolicyNames): subscriptionResourceId('Microsoft.Authorization/policyDefinitions', name.value)]

resource group 'Microsoft.Resources/resourceGroups@2024-03-01' = {
  name: groupName
  location: location
  tags: { purpose: 'boinc-acceptance', environment: 'dev', expiresAt: expiresAt }
}

module policies '../../../infra/modules/policy-assignments.bicep' = {
  name: 'boinc-dev-policies'
  scope: group
  params: {
    definitionIds: definitionIds
    definitionParameters: { '${guardrailPolicyNames.auditPublicAccess}': { effect: { value: 'deny' } } }
  }
}

module exemptions '../../../infra/modules/boinc-exemptions.bicep' = {
  name: 'boinc-dev-exemptions'
  scope: group
  dependsOn: [policies]
  params: {
    noStorageKeysAssignmentId: resourceId(subscription().subscriptionId, groupName, 'Microsoft.Authorization/policyAssignments', guardrailPolicyNames.noStorageKeys)
    noMonitorKeysAssignmentId: resourceId(subscription().subscriptionId, groupName, 'Microsoft.Authorization/policyAssignments', guardrailPolicyNames.noMonitorKeys)
  }
}

module registry 'modules/dev-registry.bicep' = {
  name: 'boinc-dev-registry'
  scope: group
  dependsOn: [exemptions]
  params: { name: registryName, location: location }
}

output resourceGroup string = group.name
output registry string = registry.outputs.name
