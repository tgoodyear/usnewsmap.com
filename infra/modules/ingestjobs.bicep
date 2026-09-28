// Ingest jobs (04 §4.4, 08 §8.4): the `usnm-ingest` image as Container Apps
// Jobs inside the VNet-integrated environment, so every data path stays on
// the private endpoints with managed identities.
//
// - `caj-usnm-ingest-{env}`: `usnm-ingest run` (enqueue from LoC's listing,
//   curate, release into Quickwit). Weekly on `cron`, or manual when it is
//   empty. The Quickwit writer it starts authenticates with the job's
//   system-assigned identity (Blob Data Contributor on `qw-index` only); the
//   pipeline itself uses `id-usnm-ingest`.
// - `caj-usnm-backfill-{env}`: manual; `workers` parallel replicas of
//   `usnm-ingest curate --enqueue`: each enqueues LoC's listing (idempotent,
//   safe in parallel), then claims batches from the Cosmos queue until none
//   are left (~22 h at 8 workers for the full corpus, 04 §4.1.1).
//   Archives are streamed, so no scratch disk is needed.

param location string
param tags object
param environmentId string
param image string
@description('Private registry the image comes from (pulled with the ingest identity).')
param registryServer string
param ingestIdentityId string
param ingestClientId string
param storageAccountName string
param storageBlobEndpoint string
param cosmosEndpoint string
param jobNameSuffix string
@description('Weekly schedule for the ingest job (UTC cron). Empty: manual only.')
param cron string = ''
@minValue(1)
@maxValue(32)
param workers int = 8

var env = [
  { name: 'USNM_COSMOS_ENDPOINT', value: cosmosEndpoint }
  { name: 'USNM_CURATED_URL', value: '${storageBlobEndpoint}curated' }
  { name: 'USNM_REFERENCE_URL', value: '${storageBlobEndpoint}reference' }
  // Selects id-usnm-ingest at the managed identity endpoint.
  { name: 'AZURE_CLIENT_ID', value: ingestClientId }
  { name: 'RUST_LOG', value: 'info' }
]

resource ingest 'Microsoft.App/jobs@2024-03-01' = {
  name: 'caj-usnm-ingest-${jobNameSuffix}'
  location: location
  tags: tags
  identity: {
    type: 'SystemAssigned,UserAssigned'
    userAssignedIdentities: { '${ingestIdentityId}': {} }
  }
  properties: {
    environmentId: environmentId
    workloadProfileName: 'Consumption'
    configuration: {
      registries: [{ server: registryServer, identity: ingestIdentityId }]
      triggerType: empty(cron) ? 'Manual' : 'Schedule'
      manualTriggerConfig: empty(cron) ? { parallelism: 1, replicaCompletionCount: 1 } : null
      scheduleTriggerConfig: empty(cron)
        ? null
        : { cronExpression: cron, parallelism: 1, replicaCompletionCount: 1 }
      // A full rebuild of the corpus can take many hours.
      replicaTimeout: 86400
      replicaRetryLimit: 0
    }
    template: {
      containers: [
        {
          name: 'ingest'
          image: image
          args: [
            'run'
            '--quickwit-bin'
            '/usr/local/bin/quickwit'
            '--quickwit-metastore'
            'azure://qw-index'
            '--quickwit-index-root'
            'azure://qw-index'
          ]
          resources: { cpu: json('4.0'), memory: '8Gi' }
          env: concat(env, [{ name: 'QW_AZURE_STORAGE_ACCOUNT', value: storageAccountName }])
        }
      ]
    }
  }
}

resource backfill 'Microsoft.App/jobs@2024-03-01' = {
  name: 'caj-usnm-backfill-${jobNameSuffix}'
  location: location
  tags: tags
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: { '${ingestIdentityId}': {} }
  }
  properties: {
    environmentId: environmentId
    workloadProfileName: 'Consumption'
    configuration: {
      registries: [{ server: registryServer, identity: ingestIdentityId }]
      triggerType: 'Manual'
      manualTriggerConfig: { parallelism: workers, replicaCompletionCount: workers }
      replicaTimeout: 86400
      // A worker that dies leaves its batch leased; another replica or run
      // picks it up once the lease expires.
      replicaRetryLimit: 1
    }
    template: {
      containers: [
        {
          name: 'curate'
          image: image
          args: ['curate', '--enqueue']
          // bzip2 decoding is single-threaded: one vCPU per worker.
          resources: { cpu: json('1.0'), memory: '2Gi' }
          env: env
        }
      ]
    }
  }
}

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageAccountName

  resource blobs 'blobServices' existing = {
    name: 'default'

    resource index 'containers' existing = {
      name: 'qw-index'
    }
  }
}

var blobContributor = 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'

// The Quickwit writer node: splits and the file-backed metastore, nothing else.
resource writerIndexContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: account::blobs::index
  name: guid(account::blobs::index.id, ingest.id, blobContributor)
  properties: {
    principalId: ingest.identity.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobContributor)
  }
}

output ingestJobName string = ingest.name
output backfillJobName string = backfill.name
