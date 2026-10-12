// The on-prem Serial Set runner's access to the data lake (#229, ADR-0008
// amendment of October 2026): `usnm-ingest congress-serialset` run on an OCR
// node, as that node's Arc machine identity, in place of the Azure job.
//
// One grant: Storage Blob Data Contributor on the curated container, with a
// condition that leaves it only what the fetch does there, all of it under
// congress/serialset/:
// - list, with a prefix under congress/serialset/ (the fetch lists only its
//   own series, congress/curated.rs finished_in and empties_in);
// - read, write and create blobs whose path is under congress/serialset/
//   (counts.json, the parquet parts and empty.json, all create-only);
// - no delete and no move, anywhere.
// Nothing on the other containers, and nothing on Cosmos: the runner holds
// its lock in a local state file (docs/operations.md, "The Serial Set
// on-prem").

param storageName string
@description('Object id of the runner\'s identity (the node\'s Arc machine identity).')
param principalId string

var blobContributor = 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
var blobs = 'Microsoft.Storage/storageAccounts/blobServices/containers/blobs'
var path = 'congress/serialset/'

// ABAC (condition version 2.0). Each clause either doesn't apply to the
// request's action or holds for it; all three must hold.
var condition = join(
  [
    '('
    ' ('
    '  !(ActionMatches{\'${blobs}/read\'} AND NOT SubOperationMatches{\'Blob.List\'})'
    '  AND !(ActionMatches{\'${blobs}/write\'})'
    '  AND !(ActionMatches{\'${blobs}/add/action\'})'
    ' )'
    ' OR'
    ' (@Resource[${blobs}:path] StringStartsWith \'${path}\')'
    ')'
    'AND'
    '('
    ' (!(ActionMatches{\'${blobs}/read\'} AND SubOperationMatches{\'Blob.List\'}))'
    ' OR'
    ' (@Request[${blobs}:prefix] StringStartsWith \'${path}\')'
    ')'
    'AND'
    '('
    ' !(ActionMatches{\'${blobs}/delete\'})'
    ' AND !(ActionMatches{\'${blobs}/move/action\'})'
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
  name: guid(account::blobService::curated.id, principalId, blobContributor, 'serialset-runner')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobContributor)
    description: 'The on-prem Serial Set runner (#229): list, read and create blobs under congress/serialset/ only; no delete.'
    conditionVersion: '2.0'
    condition: condition
  }
}

output roleAssignmentId string = grant.id
