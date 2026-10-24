// The on-prem OCR damage tables' writer (#296, 12 §12.5 Step 6): `usnm-ingest
// ocr-damage` on an on-prem host (pve-laptop CT 215, usnm-batch) as that host's own
// Arc machine identity, in place of a one-off execution of caj-usnm-ingest. It reads
// curated and reference through the on-prem hosts' group (jaocr-runner.bicep, with
// jaOcrOnPrem); this module adds only the writes.
//
// The role (damage-writer-role.bicep) can only create new blobs: no read,
// overwrite, delete or move. The tables and the versions list are written
// create-only (If-None-Match: *) anyway, so a run that finds a blob there keeps
// it. It is assigned on curated with a condition that allows the create only
// under audit/damage/ (ocr_damage::root() and its gate results).
param storageName string
@description('Object id of the writer: the on-prem host\'s own Arc machine identity (a service principal), not the on-prem group.')
param principalId string
@description('The writer role\'s definition name (damage-writer-role.bicep\'s output).')
param writerRoleName string

var blobs = 'Microsoft.Storage/storageAccounts/blobServices/containers/blobs'
var path = 'audit/damage/'

// ABAC (condition version 2.0): a create only under audit/damage/.
var condition = join(
  [
    '('
    ' !(ActionMatches{\'${blobs}/add/action\'})'
    ' OR'
    ' (@Resource[${blobs}:path] StringStartsWith \'${path}\')'
    ')'
  ],
  '\n'
)

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageName
  resource blobService 'blobServices' existing = {
    name: 'default'
    resource curated 'containers' existing = {
      name: 'curated'
    }
  }
}

resource grant 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: account::blobService::curated
  name: guid(account::blobService::curated.id, principalId, 'usnm-ocr-damage-writer')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    // Custom roles are addressed at subscription level wherever they're defined.
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', writerRoleName)
    description: 'The on-prem OCR damage tables (#296): create new blobs under audit/damage/ only; no overwrite, no delete.'
    conditionVersion: '2.0'
    condition: condition
  }
}
