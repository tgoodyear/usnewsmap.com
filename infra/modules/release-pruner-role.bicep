// The on-prem release's prune role (#1703): delete blobs, nothing else. Assigned
// only on the index account's qw-index, beside the writer role
// (modules/release-writer-role.bicep, which can't delete), so that
// `usnm-ingest prune` can remove old index versions through Quickwit.
//
// Custom, not Storage Blob Data Contributor: that built-in also carries the
// container actions (containers/delete, containers/write), which at container
// scope would let the host delete qw-index itself, and the index account has
// no container soft delete. This role has one data action.
//
// Declared whenever the release host is set, not only while the grant is on,
// for the reason the writer role is (a deployment stack can't delete a custom
// role whose assignable scope carries the stack's own deny assignment). It
// grants nothing until modules/onprem-release.bicep assigns it. The
// assignment is in that module.

@description('The role\'s name, before its scope (infra/naming.bicep).')
param roleLabel string

resource pruner 'Microsoft.Authorization/roleDefinitions@2022-04-01' = {
  name: guid(resourceGroup().id, 'release-pruner')
  properties: {
    // Role names are unique per tenant: include the subscription and group.
    roleName: '${roleLabel} (${take(subscription().subscriptionId, 8)}/${resourceGroup().name})'
    description: 'The on-prem release: delete blobs. No read, write or container actions.'
    type: 'CustomRole'
    permissions: [
      {
        actions: []
        dataActions: [
          'Microsoft.Storage/storageAccounts/blobServices/containers/blobs/delete'
        ]
      }
    ]
    assignableScopes: [resourceGroup().id]
  }
}

output name string = pruner.name
