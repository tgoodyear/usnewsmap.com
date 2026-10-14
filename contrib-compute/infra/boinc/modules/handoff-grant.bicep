// The BOINC bridge's access to the hand-off container (#469, docs/boinc.md#one-page-from-the-scan-cache):
// the on-prem OCR nodes' writer role (list, read and create; no delete) on the exchange
// account's boinc-handoff container, for submit-remote (reads dispatch/jobs/<id>/) and spool-out
// (writes spool/<id>/ and gold/trust.json). Deployed by the BOINC stack at the main group, which
// holds the account and the role (infra/modules/ocr-exchange.bicep), so the grant lives with
// the BOINC infrastructure.
param exchangeName string
@description('The bridge\'s identity (usnm-boinc-id-<env>).')
param principalId string

resource exchange 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: exchangeName

  resource blobService 'blobServices' existing = {
    name: 'default'

    resource handoff 'containers' existing = {
      name: 'boinc-handoff'
    }
  }
}

resource writer 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: exchange::blobService::handoff
  name: guid(exchange::blobService::handoff.id, principalId, 'usnm-ocr-node-writer')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    // ocr-exchange.bicep's node writer role, in the main group.
    roleDefinitionId: subscriptionResourceId(
      'Microsoft.Authorization/roleDefinitions',
      guid(resourceGroup().id, 'usnm-ocr-node-writer')
    )
  }
}
