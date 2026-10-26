// The OCR damage tables' writer role (#296): one data action, blobs/add/action,
// which creates a new block blob. Without blobs/write it can't overwrite a blob
// that exists (Put Blob needs write for that), and without delete or move it
// can't remove or rename one. No read and no control-plane actions. Declared
// whenever the writer is set, not only while ocrDamageOnPrem is on, so turning
// that off removes the grant and keeps the role: a deployment stack can't delete
// a custom role whose assignable scope carries the stack's own deny assignment
// (LinkedAuthorizationFailed, as for release-writer-role.bicep). The grant is in
// modules/damage-writer.bicep.

@description('The role\'s name, before its scope (infra/naming.bicep).')
param roleLabel string

resource writer 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(resourceGroup().id, 'usnm-ocr-damage-writer')
  properties: {
    // Role names are unique per tenant: include the subscription and group.
    roleName: '${roleLabel} (${take(subscription().subscriptionId, 8)}/${resourceGroup().name})'
    description: 'The on-prem OCR damage tables: create new blobs only. No read, overwrite, delete or container actions.'
    type: 'CustomRole'
    permissions: [
      {
        actions: []
        dataActions: [
          'Microsoft.Storage/storageAccounts/blobServices/containers/blobs/add/action'
        ]
      }
    ]
    assignableScopes: [resourceGroup().id]
  }
}

output name string = writer.name
