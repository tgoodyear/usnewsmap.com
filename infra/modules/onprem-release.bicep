// The on-prem release host's access (#283, #917; docs/operations/rebuild-and-index-storage.md, "Full
// rebuild on-prem"): `usnm-ingest release` built on the release container
// (usnm-release-arcs-<env>) with the Quickwit writer on its own disk, then
// copied into Azure with `usnm-ingest publish-staged`, as that container's
// Arc machine identity. Its own identity, not the on-prem hosts' group:
// only the release writes the serving index.
//
// - curated: Storage Blob Data Reader (the release reads every batch);
// - reference: the release writer role (modules/release-writer-role.bicep:
//   blob read, write, create; no delete and no container actions), writes
//   only under pages-v*/ (a version's snapshot, create-only in the code) and
//   to index-storage-pending.json.
//   Never current.json: promotion stays the operator's step;
// - the index account's qw-index: the same role, unconditioned (splits,
//   each index's metastore.json and the metastore's manifest.json);
// - with pruneRoleName (#1703, USNM_ONPREM_RELEASE_PRUNE), the index account's qw-index
//   also gets the prune role (modules/release-pruner-role.bicep: blob delete,
//   nothing else), conditioned to blobs under pages-* and congress-*, the
//   index ids `usnm-ingest prune` may delete (crates/usnm-ingest/src/prune.rs,
//   PREFIXES). Not the metastore's own files at the container root, and no
//   other container or account;
// - Cosmos: Data Contributor on the index_runs container only (#1096, ADR-0027
//   settled decision 5): `usnm-ingest import-staged-run` records the
//   published version's run, so /v1/versions and /datasets list it. Not ops
//   (the writer lock, ops/current) and no other container. The built-in role
//   also deletes items in index_runs. It reaches Cosmos only over the
//   account's IP firewall, which this module doesn't open: onPremServing or
//   cosmosOnPremAccess does, with ocrNodeIps (main.bicep);
// - the registry: AcrPull, for the pinned Quickwit image
//   (scripts/onprem/install-quickwit.sh).

param storageName string
param indexAccountName string
param registryName string
param cosmosName string
@description('Object id of the release container\'s Arc machine identity.')
param principalId string
@description('Name (a GUID) of the release writer role (modules/release-writer-role.bicep).')
param writerRoleName string
@description('Name (a GUID) of the prune role (modules/release-pruner-role.bicep). Empty: no delete grant.')
param pruneRoleName string = ''
var blobs = 'Microsoft.Storage/storageAccounts/blobServices/containers/blobs'
var blobDataReader = '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'
var acrPull = '7f951dda-4ed3-4680-a7ca-43fe172d538d'
var cosmosDataContributor = '00000000-0000-0000-0000-000000000002'

// ABAC (condition version 2.0): writes and creates in reference only to a
// version's snapshot or the staged pointer.
var referenceCondition = join(
  [
    '('
    ' !(ActionMatches{\'${blobs}/write\'})'
    ' AND !(ActionMatches{\'${blobs}/add/action\'})'
    ')'
    'OR'
    '('
    ' @Resource[${blobs}:path] StringStartsWith \'pages-v\''
    ' OR @Resource[${blobs}:path] StringEquals \'index-storage-pending.json\''
    ')'
  ],
  '\n'
)

// ABAC (condition version 2.0): deletes only of blobs under an index prefix
// prune may delete (PREFIXES in prune.rs).
// Same form as referenceCondition. The prune role has the delete data action and no other, so
// the first clause (any other action) has nothing to match; it is what the form needs to be valid.
var pruneCondition = join(
  [
    '('
    ' !(ActionMatches{\'${blobs}/delete\'})'
    ')'
    'OR'
    '('
    ' @Resource[${blobs}:path] StringStartsWith \'pages-\''
    ' OR @Resource[${blobs}:path] StringStartsWith \'congress-\''
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

    resource reference 'containers' existing = {
      name: 'reference'
    }
  }
}

resource index 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: indexAccountName

  resource blobService 'blobServices' existing = {
    name: 'default'

    resource qwIndex 'containers' existing = {
      name: 'qw-index'
    }
  }
}

resource cosmos 'Microsoft.DocumentDB/databaseAccounts@2024-11-15' existing = {
  name: cosmosName
}

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' existing = {
  name: registryName
}

resource curatedRead 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::curated
  name: guid(lake::blobService::curated.id, principalId, blobDataReader)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobDataReader)
    description: 'The on-prem release (#283): reads every curated batch.'
  }
}

resource referenceWrite 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::reference
  name: guid(lake::blobService::reference.id, principalId, 'usnm-release-writer')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    // Custom roles are addressed at subscription level wherever they're defined.
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', writerRoleName)
    description: 'The on-prem release (#283): reads reference; writes only pages-v*/ and index-storage-pending.json; no delete.'
    conditionVersion: '2.0'
    condition: referenceCondition
  }
}

resource indexWrite 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: index::blobService::qwIndex
  name: guid(index::blobService::qwIndex.id, principalId, 'usnm-release-writer')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', writerRoleName)
    description: 'The on-prem release (#283): copies staged indexes into qw-index; no delete.'
  }
}

resource indexPrune 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(pruneRoleName)) {
  scope: index::blobService::qwIndex
  name: guid(index::blobService::qwIndex.id, principalId, 'release-pruner')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', pruneRoleName)
    description: 'The on-prem release (#1703): deletes blobs of old index versions (pages-*, congress-*) in qw-index, for usnm-ingest prune.'
    conditionVersion: '2.0'
    condition: pruneCondition
  }
}

resource indexRunsWrite 'Microsoft.DocumentDB/databaseAccounts/sqlRoleAssignments@2024-11-15' = {
  parent: cosmos
  name: guid(cosmos.id, principalId, cosmosDataContributor, 'index_runs')
  properties: {
    principalId: principalId
    roleDefinitionId: '${cosmos.id}/sqlRoleDefinitions/${cosmosDataContributor}'
    scope: '${cosmos.id}/dbs/usnm/colls/index_runs'
  }
}

resource pull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: registry
  name: guid(registry.id, principalId, acrPull)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPull)
    description: 'The on-prem release (#283): pulls the pinned Quickwit image.'
  }
}
