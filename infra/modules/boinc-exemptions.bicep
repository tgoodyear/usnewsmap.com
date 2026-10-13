// Waivers for the BOINC server stack (ADR-0019, decision 5), each scoped to
// one resource and one policy. The Container Apps Azure Files mount takes the
// account key, and the environment's log configuration takes the workspace's
// shared key, so the two deny policies would otherwise refuse the stack.
// The policy definitions are unchanged and still apply to everything else.

@description('Name of the BOINC Azure Files storage account (contrib-compute/infra/boinc). Empty: no exemption.')
param filesAccountName string = ''
@description('Name of the BOINC Log Analytics workspace (usnm-<name>-log). Empty: no exemption.')
param workspaceName string = ''
@description('Name of the shared-key storage policy assignment in this group.')
param noStorageKeysAssignmentName string
@description('Name of the monitor local-auth policy assignment in this group.')
param noMonitorKeysAssignmentName string

resource filesAccount 'Microsoft.Storage/storageAccounts@2023-05-01' existing = if (!empty(filesAccountName)) {
  name: filesAccountName
}

resource workspace 'Microsoft.OperationalInsights/workspaces@2023-09-01' existing = if (!empty(workspaceName)) {
  name: workspaceName
}

resource filesExemption 'Microsoft.Authorization/policyExemptions@2022-07-01-preview' = if (!empty(filesAccountName)) {
  scope: filesAccount
  name: 'usnm-boinc-files-shared-key'
  properties: {
    policyAssignmentId: resourceId('Microsoft.Authorization/policyAssignments', noStorageKeysAssignmentName)
    exemptionCategory: 'Waiver'
    displayName: 'BOINC Files account: shared key access'
    description: 'The Container Apps Azure Files mount takes the account key. The account is private-endpoint only and the key stays in the platform configuration. ADR-0019 decision 5, ADR-0009 addendum.'
  }
}

resource workspaceExemption 'Microsoft.Authorization/policyExemptions@2022-07-01-preview' = if (!empty(workspaceName)) {
  scope: workspace
  name: 'usnm-boinc-log-local-auth'
  properties: {
    policyAssignmentId: resourceId('Microsoft.Authorization/policyAssignments', noMonitorKeysAssignmentName)
    exemptionCategory: 'Waiver'
    displayName: 'BOINC Log Analytics workspace: shared key'
    description: 'The Container Apps environment log configuration takes the workspace shared key. The key stays in the platform configuration. ADR-0019 decision 5, ADR-0009 addendum.'
  }
}
