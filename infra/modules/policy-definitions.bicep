// Guardrails (08 §8.5, ADR-0005, ADR-0008), defined at subscription scope and
// assigned to the project resource groups by policy-assignments.bicep.

targetScope = 'subscription'

resource noIaas 'Microsoft.Authorization/policyDefinitions@2023-04-01' = {
  name: 'usnm-deny-iaas-compute'
  properties: {
    displayName: 'US News Map: no VMs, scale sets, AKS or Batch accounts'
    policyType: 'Custom'
    mode: 'All'
    policyRule: {
      if: {
        field: 'type'
        in: [
          'Microsoft.Compute/virtualMachines'
          'Microsoft.Compute/virtualMachineScaleSets'
          'Microsoft.ContainerService/managedClusters'
          'Microsoft.Batch/batchAccounts'
        ]
      }
      then: { effect: 'deny' }
    }
  }
}

resource noSharedKey 'Microsoft.Authorization/policyDefinitions@2023-04-01' = {
  name: 'usnm-deny-storage-shared-key'
  properties: {
    displayName: 'US News Map: storage accounts must disable shared key access'
    policyType: 'Custom'
    mode: 'Indexed'
    policyRule: {
      if: {
        allOf: [
          { field: 'type', equals: 'Microsoft.Storage/storageAccounts' }
          { field: 'Microsoft.Storage/storageAccounts/allowSharedKeyAccess', notEquals: false }
        ]
      }
      then: { effect: 'deny' }
    }
  }
}

resource noCosmosKeys 'Microsoft.Authorization/policyDefinitions@2023-04-01' = {
  name: 'usnm-deny-cosmos-local-auth'
  properties: {
    displayName: 'US News Map: Cosmos DB accounts must disable local (key) auth'
    policyType: 'Custom'
    mode: 'Indexed'
    policyRule: {
      if: {
        allOf: [
          { field: 'type', equals: 'Microsoft.DocumentDB/databaseAccounts' }
          { field: 'Microsoft.DocumentDB/databaseAccounts/disableLocalAuth', notEquals: true }
        ]
      }
      then: { effect: 'deny' }
    }
  }
}

resource auditPublicAccess 'Microsoft.Authorization/policyDefinitions@2023-04-01' = {
  name: 'usnm-audit-data-public-network'
  properties: {
    displayName: 'US News Map: data services should have public network access disabled'
    description: 'Audit only: the network-guard job closes public access outside an open backfill window. The public tiles account is tagged usnm-public=true and exempt.'
    policyType: 'Custom'
    mode: 'Indexed'
    policyRule: {
      if: {
        allOf: [
          { field: 'tags[\'usnm-public\']', notEquals: 'true' }
          {
            anyOf: [
              {
                allOf: [
                  { field: 'type', equals: 'Microsoft.Storage/storageAccounts' }
                  { field: 'Microsoft.Storage/storageAccounts/publicNetworkAccess', notEquals: 'Disabled' }
                ]
              }
              {
                allOf: [
                  { field: 'type', equals: 'Microsoft.DocumentDB/databaseAccounts' }
                  { field: 'Microsoft.DocumentDB/databaseAccounts/publicNetworkAccess', notEquals: 'Disabled' }
                ]
              }
            ]
          }
        ]
      }
      then: { effect: 'audit' }
    }
  }
}

// Every resource type in the project that has resource logs must send some
// to a workspace (the settings live in diagnostics.bicep). Audit, not
// deployIfNotExists: the template writes them, and chooses categories per
// resource to stay inside the workspace's daily cap (a remediation identity
// would need role-assignment rights and could only turn on whole category
// groups). Mode All, so storage services (child resources) are evaluated.
var loggedTypes = [
  'Microsoft.OperationalInsights/workspaces'
  'Microsoft.ContainerRegistry/registries'
  'Microsoft.Network/virtualNetworks'
  'Microsoft.App/managedEnvironments'
  'Microsoft.DocumentDB/databaseAccounts'
  'Microsoft.Storage/storageAccounts/blobServices'
  'Microsoft.Storage/storageAccounts/queueServices'
  'Microsoft.Storage/storageAccounts/tableServices'
  'Microsoft.Storage/storageAccounts/fileServices'
]

resource auditDiagnostics 'Microsoft.Authorization/policyDefinitions@2023-04-01' = {
  name: 'usnm-audit-diagnostic-settings'
  properties: {
    displayName: 'US News Map: resources with resource logs must send them to Log Analytics'
    description: 'Audit only: infra/modules/diagnostics.bicep writes the settings.'
    policyType: 'Custom'
    mode: 'All'
    policyRule: {
      if: { field: 'type', in: loggedTypes }
      then: {
        effect: 'auditIfNotExists'
        details: {
          type: 'Microsoft.Insights/diagnosticSettings'
          existenceCondition: {
            allOf: [
              { field: 'Microsoft.Insights/diagnosticSettings/workspaceId', exists: true }
              {
                count: {
                  field: 'Microsoft.Insights/diagnosticSettings/logs[*]'
                  where: { field: 'Microsoft.Insights/diagnosticSettings/logs[*].enabled', equals: 'true' }
                }
                greater: 0
              }
            ]
          }
        }
      }
    }
  }
}

output ids array = [noIaas.id, noSharedKey.id, noCosmosKeys.id, auditPublicAccess.id, auditDiagnostics.id]
