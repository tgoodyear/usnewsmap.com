// The search log copy host's access (#1097; ADR-0027, settled decision 3): a
// daily copy of searches/days/ to a restricted dataset on tank, made with
// `usnm-ingest searchlog-copy` (scripts/onprem/searchlog-copy/README.md) as
// the Arc machine identity of the host that writes that dataset. Its own
// identity, not the on-prem hosts' group and not the serving container: any
// process of the machine that can reach the Arc agent's token endpoint can use
// the grant, and the serving container mounts nothing from tank.
//
// - searches: Storage Blob Data Reader, container-wide and with no condition:
//   the copy lists days/ and reads the day files, and writes and deletes
//   nothing. This is what USNM_SEARCH_LOG_READERS grants (rbac.bicep), so the
//   copy reads no more than the operators who may read the search log (the
//   same readers, ADR-0012). It could read staging/ too. A path condition
//   would need the list operation's prefix attribute, which can't be tried
//   here; narrowing it is a later change.
//
// Use either this or an entry in USNM_SEARCH_LOG_READERS for the host. The
// assignment is named as rbac.bicep names a reader's, so listing the host in
// both would deploy one assignment twice, not a second one; one is enough.

param storageName string
@description('Object id of the copy host\'s Arc machine identity.')
param principalId string

var blobDataReader = '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'

resource lake 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageName

  resource blobService 'blobServices' existing = {
    name: 'default'

    resource searches 'containers' existing = {
      name: 'searches'
    }
  }
}

resource searchLogRead 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::searches
  name: guid(lake::blobService::searches.id, principalId, blobDataReader)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobDataReader)
    description: 'Search log copy (#1097): reads the day files for the restricted dataset on tank; no write, no delete.'
  }
}
