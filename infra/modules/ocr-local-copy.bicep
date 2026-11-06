// The OCR output copy's access: a daily copy of curated/ocr-local/ to a
// dataset on tank, made with `usnm-ingest ocr-local-copy`
// (scripts/onprem/ocr-local-copy/README.md) as the copy host's Arc machine
// identity, the one the search log copy uses.
//
// - curated: Storage Blob Data Reader, conditioned to read and list only
//   ocr-local/. The copy writes and deletes nothing.

param storageName string
@description('Object id of the copy host\'s Arc machine identity.')
param principalId string

var blobDataReader = '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'
var blobs = 'Microsoft.Storage/storageAccounts/blobServices/containers/blobs'
var path = 'ocr-local/'

// ABAC (condition version 2.0). Each clause either doesn't apply to the
// request's action or holds for it; both must hold.
var condition = join(
  [
    '('
    ' (!(ActionMatches{\'${blobs}/read\'} AND NOT SubOperationMatches{\'Blob.List\'}))'
    ' OR'
    ' (@Resource[${blobs}:path] StringStartsWith \'${path}\')'
    ')'
    'AND'
    '('
    ' (!(ActionMatches{\'${blobs}/read\'} AND SubOperationMatches{\'Blob.List\'}))'
    ' OR'
    ' (@Request[${blobs}:prefix] StringStartsWith \'${path}\')'
    ')'
  ],
  '\n'
)

resource lake 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageName

  resource blobService 'blobServices' existing = {
    name: 'default'

    resource curated 'containers' existing = {
      name: 'curated'
    }
  }
}

resource ocrLocalRead 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::curated
  name: guid(lake::blobService::curated.id, principalId, 'usnm-ocr-local-copy')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobDataReader)
    description: 'OCR output copy: reads ocr-local/ for the dataset on tank; no write, no delete.'
    conditionVersion: '2.0'
    condition: condition
  }
}
