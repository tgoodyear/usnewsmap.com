// The on-prem release's writer role (#283, #917): list, read and create or
// replace blobs; no delete and no container actions. Declared whenever the
// release host is set, not only while onPremRelease is on, so turning the
// release off removes its grants and keeps the role: a deployment stack
// can't delete a custom role whose assignable scope carries the stack's own
// deny assignment (LinkedAuthorizationFailed). The grants are in
// modules/onprem-release.bicep.

@description('The role\'s name, before its scope (infra/naming.bicep).')
param roleLabel string

resource writer 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(resourceGroup().id, 'usnm-release-writer')
  properties: {
    // Role names are unique per tenant: include the subscription and group.
    roleName: '${roleLabel} (${take(subscription().subscriptionId, 8)}/${resourceGroup().name})'
    description: 'The on-prem release: list, read and create or replace blobs. No delete, no container actions.'
    type: 'CustomRole'
    permissions: [
      {
        actions: []
        dataActions: [
          'Microsoft.Storage/storageAccounts/blobServices/containers/blobs/read'
          'Microsoft.Storage/storageAccounts/blobServices/containers/blobs/write'
          'Microsoft.Storage/storageAccounts/blobServices/containers/blobs/add/action'
        ]
      }
    ]
    assignableScopes: [resourceGroup().id]
  }
}

output name string = writer.name
