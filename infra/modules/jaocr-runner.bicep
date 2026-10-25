// The on-prem Japanese OCR's access to the data lake (#917, #128): ocr/ja-ocr/jaocr.py `run`
// on an on-prem host (scripts/ocr-node/ja-ocr-onprem.sh) as that host's Arc machine
// identity, in place of the Azure job caj-usnm-jaocr. The principal is the on-prem hosts'
// group (usnm-onprem-grp-<env>), so any of them can run it.
//
// A custom role with blob read and write data actions and no control-plane actions (as
// for the Serial Set: Storage Blob Data Contributor would also let it delete or
// reconfigure the container, which a condition can't restrict), assigned twice:
// - curated: list and read anywhere (the job reads the published batches' Parquet parts),
//   write and create only under ocr-ja/ (targets, claims, ALTO, page parts; the claim
//   takeover is an ETag compare-and-swap write, not a delete);
// - reference: list and read anywhere (current.json, batches.json, titles.json), write
//   and create only status/ocr-ja.json (the status page's progress).
// No delete and no move anywhere: the role has neither data action. Nothing on the other
// containers or on Cosmos.
param storageName string
@description('Object id of the runners\' principal: the on-prem hosts\' Entra group.')
param principalId string
@description('The writer role\'s name, before its scope (infra/naming.bicep).')
param roleLabel string

var blobs = 'Microsoft.Storage/storageAccounts/blobServices/containers/blobs'

resource writer 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(resourceGroup().id, 'usnm-jaocr-writer')
  properties: {
    // Role names are unique per tenant: include the subscription and group.
    roleName: '${roleLabel} (${take(subscription().subscriptionId, 8)}/${resourceGroup().name})'
    description: 'The on-prem Japanese OCR: list, read and create blobs. No delete, no container actions.'
    type: 'CustomRole'
    permissions: [
      {
        actions: []
        dataActions: [
          '${blobs}/read'
          '${blobs}/write'
          '${blobs}/add/action'
        ]
      }
    ]
    assignableScopes: [resourceGroup().id]
  }
}

// ABAC (condition version 2.0): reads and lists anywhere in the container; a write or
// create only where `writable` holds.
func writesOnly(blobs string, writable string) string =>
  join(
    [
      '('
      ' ('
      '  !(ActionMatches{\'${blobs}/write\'})'
      '  AND !(ActionMatches{\'${blobs}/add/action\'})'
      ' )'
      ' OR'
      ' (${writable})'
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
    resource reference 'containers' existing = {
      name: 'reference'
    }
  }
}

resource curatedGrant 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: account::blobService::curated
  name: guid(account::blobService::curated.id, principalId, 'usnm-jaocr-writer')
  properties: {
    principalId: principalId
    principalType: 'Group'
    // Custom roles are addressed at subscription level wherever they're defined.
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', writer.name)
    description: 'The on-prem Japanese OCR (#917): read curated, create and write under ocr-ja/ only; no delete.'
    conditionVersion: '2.0'
    condition: writesOnly(blobs, '@Resource[${blobs}:path] StringStartsWith \'ocr-ja/\'')
  }
}

resource referenceGrant 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: account::blobService::reference
  name: guid(account::blobService::reference.id, principalId, 'usnm-jaocr-writer')
  properties: {
    principalId: principalId
    principalType: 'Group'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', writer.name)
    description: 'The on-prem Japanese OCR (#917): read reference, write status/ocr-ja.json only; no delete.'
    conditionVersion: '2.0'
    condition: writesOnly(blobs, '@Resource[${blobs}:path] StringEquals \'status/ocr-ja.json\'')
  }
}
