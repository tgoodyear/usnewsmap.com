// The on-prem BOINC batch controller (#866, docs/boinc-batch-scheduler.md): the scan
// fetcher's identity (CT 211's Arc machine identity) runs scripts/boinc/batch_scheduler.py,
// which starts the hand-off job (main group) and the bridge job (BOINC group) through
// scripts/boinc/start-boinc-bridge.sh. That launcher writes each run's settings into the job
// (`az containerapp job update`) and starts it, so the runner needs:
// - Reader on the subscription: the launcher reads the main stack's outputs (a
//   subscription deployment stack), the BOINC stack's, both groups, the jobs and their
//   executions;
// - a custom role on the main and BOINC groups with the job update and start actions and
//   the linked checks an update makes (join the job's environment, assign its identity).
// Its blob access to boinc-handoff and its log read are ocr-exchange.bicep and
// boinc-handoff-logs.bicep. Accepted: the runner can change those jobs and so act as their
// identities (#866).
targetScope = 'subscription'

@description('Object id of the runner\'s identity (the scan-fetch container\'s Arc machine identity).')
param principalId string
@description('The controller role\'s name, before its scope (infra/naming.bicep).')
param roleLabel string
@description('The groups holding the jobs: the main group and the BOINC group.')
param groups array

var reader = 'acdd72a7-3385-48ef-bd42-f606fba81ae7'

resource role 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(subscription().id, 'boinc-controller-role')
  properties: {
    roleName: '${roleLabel} (${take(subscription().subscriptionId, 8)})'
    description: 'The on-prem BOINC batch controller: update and start the BOINC hand-off and bridge jobs.'
    type: 'CustomRole'
    permissions: [
      {
        actions: [
          'Microsoft.App/jobs/read'
          'Microsoft.App/jobs/write'
          'Microsoft.App/jobs/start/action'
          'Microsoft.App/jobs/executions/read'
          'Microsoft.App/jobs/execution/read'
          'Microsoft.App/managedEnvironments/read'
          'Microsoft.App/managedEnvironments/join/action'
          'Microsoft.ManagedIdentity/userAssignedIdentities/read'
          'Microsoft.ManagedIdentity/userAssignedIdentities/assign/action'
        ]
        notActions: []
      }
    ]
    assignableScopes: [for g in groups: subscriptionResourceId('Microsoft.Resources/resourceGroups', g)]
  }
}

resource read 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(subscription().id, principalId, reader)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', reader)
  }
}

module grants 'boinc-controller-grant.bicep' = [
  for g in groups: {
    scope: resourceGroup(g)
    name: 'boinc-controller-${uniqueString(g)}'
    params: {
      principalId: principalId
      roleId: role.id
    }
  }
]
