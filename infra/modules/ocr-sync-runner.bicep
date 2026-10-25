// The on-prem OCR pickup's access (#917, 12 §12.5 Step 8): `usnm-ingest ocr-sync`
// hourly, and ocr-seed, ocr-enqueue and ocr-requeue by hand, on the batch container
// (pve-laptop CT 215, usnm-batch) as that container's own Arc machine identity, in
// place of the Azure job usnm-ocrs-caj-<env> and its identity id-usnm-ingest. Its own
// identity, not the on-prem hosts' group: only one host consumes ocr-out and the dead
// letters. Only what those commands touch (crates/usnm-ocr-local, sync.rs):
// - curated: the lake role (read, write, create; no delete), list and read anywhere
//   (the ledger, the damage tables, the batches' parts, import records), writes and
//   creates only under ocr-local/ (ocr_local::ROOT);
// - reference: the lake role, read anywhere, write only status/ocr-local.json;
// - ocr-requests: the lake role, unconditioned: it empties each day's request file
//   once queued and creates completed/<day>.json; no delete;
// - ocr-out and ocr-quarantine: the exchange role (read, write, create, delete; no
//   container actions): parts are deleted with If-Match once imported or quarantined,
//   and --from-quarantine puts rejects back;
// - queues: Message Processor on ocr-deadletter (drained), Message Sender on all three
//   (the seeder and hit requests on ocr-priority and ocr-bulk; --from-quarantine puts
//   rejected dead letters back on ocr-deadletter), Queue Data Reader on all three (the
//   approximate counts in the status file and the seeder's refill);
// - Cosmos: Built-in Data Contributor on `ops` alone (the ocr-sync lease and
//   ops/current);
// - Application Insights: Monitoring Metrics Publisher (traces and metrics);
// - the registry: AcrPull, to install the released usnm-ingest from its image by
//   digest (home-infra scripts/lab/batch-ct-ingest.sh).
// With `promote` (ingestOnPrem), `import-staged-run --current`, which records a staged
// run and moves ops/current under the writer lock: Data Contributor on `index_runs` too.
// With `backfill` (backfillOnPrem), curation (`curate --enqueue`, `enqueue`): the curated
// grant also creates under pages/ (curation writes each attempt create-only), and Data
// Contributor on `batches` and `issues` (the work queue and the issues it curates).
// Nothing for a raw store: prod keeps none (USNM_RETAIN_RAW, USNM_ARCHIVE_ACCOUNT unset).
param storageName string
param exchangeName string
param cosmosName string
param appInsightsName string
param registryName string
@description('Object id of the batch container\'s Arc machine identity.')
param principalId string
@description('Also import-staged-run --current: Cosmos index_runs.')
param promote bool = false
@description('Also backfill curation: create under curated/pages/, Cosmos batches and issues.')
param backfill bool = false
@description('The lake role\'s name, a GUID (ocr-sync-runner-role.bicep).')
param lakeRoleName string
@description('The exchange role\'s name, a GUID (ocr-sync-runner-role.bicep).')
param exchangeRoleName string

var blobs = 'Microsoft.Storage/storageAccounts/blobServices/containers/blobs'
var queueMessageProcessor = '8a0f0c08-91a1-4084-bc3d-661d67233fed'
var queueMessageSender = 'c6a89b2d-59bc-44d0-9896-0f6e12d7b80a'
var queueDataReader = '19e7f393-937e-4f77-808e-94535e297925'
var cosmosDataContributor = '00000000-0000-0000-0000-000000000002'
var metricsPublisher = '3913510d-42f4-4e42-8a64-420c390055eb'
var acrPull = '7f951dda-4ed3-4680-a7ca-43fe172d538d'

// ABAC (condition version 2.0): reads and lists anywhere in the container; a write or
// create only where `writable` holds (as in jaocr-runner.bicep).
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
    resource requests 'containers' existing = {
      name: 'ocr-requests'
    }
  }
}

resource exchange 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: exchangeName
  resource blobService 'blobServices' existing = {
    name: 'default'
  }
  resource queueService 'queueServices' existing = {
    name: 'default'
  }
}

// ocr-out and ocr-quarantine.
resource exchangeContainers 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' existing = [
  for c in ['ocr-out', 'ocr-quarantine']: {
    parent: exchange::blobService
    name: c
  }
]

// ocr-priority and ocr-bulk, then ocr-deadletter.
var queueNames = ['ocr-priority', 'ocr-bulk', 'ocr-deadletter']
resource queues 'Microsoft.Storage/storageAccounts/queueServices/queues@2023-05-01' existing = [
  for q in queueNames: {
    parent: exchange::queueService
    name: q
  }
]

resource curatedGrant 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::curated
  name: guid(lake::blobService::curated.id, principalId, 'usnm-ocr-sync-lake')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    // Custom roles are addressed at subscription level wherever they're defined.
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', lakeRoleName)
    description: 'The on-prem OCR pickup (#917): read curated, write and create under ocr-local/ only; no delete.'
    conditionVersion: '2.0'
    condition: writesOnly(
      blobs,
      backfill
        ? '@Resource[${blobs}:path] StringStartsWith \'ocr-local/\' OR @Resource[${blobs}:path] StringStartsWith \'pages/\''
        : '@Resource[${blobs}:path] StringStartsWith \'ocr-local/\''
    )
  }
}

resource referenceGrant 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::reference
  name: guid(lake::blobService::reference.id, principalId, 'usnm-ocr-sync-lake')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', lakeRoleName)
    description: 'The on-prem OCR pickup (#917): read reference, write status/ocr-local.json only; no delete.'
    conditionVersion: '2.0'
    condition: writesOnly(blobs, '@Resource[${blobs}:path] StringEquals \'status/ocr-local.json\'')
  }
}

resource requestsGrant 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::requests
  name: guid(lake::blobService::requests.id, principalId, 'usnm-ocr-sync-lake')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', lakeRoleName)
    description: 'The on-prem OCR pickup (#917): read the hit requests, empty a queued day, mark it completed; no delete.'
  }
}

resource exchangeGrants 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (c, i) in ['ocr-out', 'ocr-quarantine']: {
    scope: exchangeContainers[i]
    name: guid(exchangeContainers[i].id, principalId, 'usnm-ocr-sync-exchange')
    properties: {
      principalId: principalId
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', exchangeRoleName)
      description: 'The on-prem OCR pickup (#917): check, import and delete node output; quarantine and replay rejects.'
    }
  }
]

resource drainDeadLetters 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: queues[2]
  name: guid(queues[2].id, principalId, queueMessageProcessor)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', queueMessageProcessor)
  }
}

resource sendWork 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (q, i) in queueNames: {
    scope: queues[i]
    name: guid(queues[i].id, principalId, queueMessageSender)
    properties: {
      principalId: principalId
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', queueMessageSender)
    }
  }
]

resource readQueues 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (q, i) in queueNames: {
    scope: queues[i]
    name: guid(queues[i].id, principalId, queueDataReader)
    properties: {
      principalId: principalId
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', queueDataReader)
    }
  }
]

resource cosmos 'Microsoft.DocumentDB/databaseAccounts@2024-11-15' existing = {
  name: cosmosName
}

// ops always; index_runs to promote; batches and issues to curate.
var stateContainers = concat(['ops'], promote ? ['index_runs'] : [], backfill ? ['batches', 'issues'] : [])

resource state 'Microsoft.DocumentDB/databaseAccounts/sqlRoleAssignments@2024-11-15' = [
  for c in stateContainers: {
    parent: cosmos
    name: guid(cosmos.id, principalId, cosmosDataContributor, c)
    properties: {
      principalId: principalId
      roleDefinitionId: '${cosmos.id}/sqlRoleDefinitions/${cosmosDataContributor}'
      scope: '${cosmos.id}/dbs/usnm/colls/${c}'
    }
  }
]

resource appInsights 'Microsoft.Insights/components@2020-02-02' existing = {
  name: appInsightsName
}

resource telemetry 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: appInsights
  name: guid(appInsights.id, principalId, metricsPublisher)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', metricsPublisher)
  }
}

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' existing = {
  name: registryName
}

resource pull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: registry
  name: guid(registry.id, principalId, acrPull)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPull)
  }
}
