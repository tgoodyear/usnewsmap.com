// The on-prem OCR pickup's roles (#917): `usnm-ingest ocr-sync` and its one-off
// siblings (ocr-seed, ocr-enqueue, ocr-requeue) on the batch container (pve-laptop
// CT 215, usnm-batch) as its own Arc machine identity, in place of the Azure job
// usnm-ocrs-caj-<env>. Two custom roles, both with data actions only (no container
// or control-plane actions, which Storage Blob Data Contributor would add):
// - lake: blob read, write and create, no delete (curated, reference, ocr-requests;
//   conditions in ocr-sync-runner.bicep keep the writes to ocr-sync's own paths);
// - exchange: the same and delete (ocr-out, ocr-quarantine: ocr-sync deletes each
//   node part once it is in curated or quarantine).
// Declared whenever the batch host is set, not only while ocrSyncOnPrem is on, so
// turning that off removes the grants and keeps the roles: a deployment stack can't
// delete a custom role whose assignable scope carries the stack's own deny assignment
// (LinkedAuthorizationFailed, as for damage-writer-role.bicep).

@description('The lake role\'s name, before its scope (infra/naming.bicep).')
param lakeRoleLabel string
@description('The exchange role\'s name, before its scope (infra/naming.bicep).')
param exchangeRoleLabel string

var blobs = 'Microsoft.Storage/storageAccounts/blobServices/containers/blobs'

resource lake 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(resourceGroup().id, 'usnm-ocr-sync-lake')
  properties: {
    // Role names are unique per tenant: include the subscription and group.
    roleName: '${lakeRoleLabel} (${take(subscription().subscriptionId, 8)}/${resourceGroup().name})'
    description: 'The on-prem OCR pickup on the data lake: list, read, write and create blobs. No delete, no container actions.'
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

resource exchange 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(resourceGroup().id, 'usnm-ocr-sync-exchange')
  properties: {
    roleName: '${exchangeRoleLabel} (${take(subscription().subscriptionId, 8)}/${resourceGroup().name})'
    description: 'The on-prem OCR pickup on the exchange account: list, read, write, create and delete blobs. No container actions.'
    type: 'CustomRole'
    permissions: [
      {
        actions: []
        dataActions: [
          '${blobs}/read'
          '${blobs}/write'
          '${blobs}/add/action'
          '${blobs}/delete'
        ]
      }
    ]
    assignableScopes: [resourceGroup().id]
  }
}

output lakeName string = lake.name
output exchangeName string = exchange.name
