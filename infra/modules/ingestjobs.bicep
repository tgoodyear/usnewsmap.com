// Ingest jobs (04 §4.4, 08 §8.4): the `usnm-ingest` image as Container Apps
// Jobs inside the VNet-integrated environment, so every data path stays on
// the private endpoints with managed identities.
//
// - `caj-usnm-ingest-{env}`: `usnm-ingest run` (enqueue from LoC's listing,
//   curate, titles-sync, release into Quickwit). Weekly on `cron`, or manual
//   when it is empty. This is how batches LoC publishes after the backfill
//   reach the site. The Quickwit writer it starts authenticates with the
//   job's system-assigned identity (Blob Data Contributor on `qw-index`
//   only); the pipeline itself uses `id-usnm-ingest`.
// - `caj-usnm-backfill-{env}`: manual, for the initial corpus (or a large
//   LoC publication); `workers` parallel replicas of `usnm-ingest curate
//   --enqueue`: each enqueues LoC's listing (idempotent, safe in parallel),
//   then claims batches from the Cosmos queue until none are left or its
//   max runtime has passed. LoC's download limit sets the pace: about 2.5
//   days for the full corpus (04 §4.4), so the job is started again after
//   each execution until the queue is empty (or on `backfillCron`, while a
//   backfill lasts). Archives are streamed, so no scratch disk is needed.
//
// The ingest job's Quickwit writer keeps its data on an NFS share
// (ingest-scratch.bicep) when `scratchStorageName` is set: indexing and
// merging a large index needs more disk than a replica has (08 §8.4).

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
@description('Application Insights connection string: the ingestion endpoint and instrumentation key. The jobs authenticate with id-usnm-ingest (Monitoring Metrics Publisher); the component accepts nothing else.')
param appInsightsConnectionString string
param jobNameSuffix string
@description('Weekly schedule for the ingest job (UTC cron). Empty: manual only.')
param cron string = ''
@description('Make every ingest run a full release: a new base from every curated batch, merged (04 §4.7). For a one-off rebuild; turn it off afterwards.')
param full bool = false
@minValue(1)
@maxValue(32)
param workers int = 8
@description('Schedule for the backfill job (UTC cron), e.g. daily while a backfill lasts. Empty: manual only.')
param backfillCron string = ''
@description('The environment storage (an NFS share) for the writer\'s data; empty: the replica\'s own disk.')
param scratchStorageName string = ''
@description('The share\'s size in GiB: the release refuses to start with less than three quarters of it free.')
param scratchGiB int = 0
@description('An image that runs as root, to hand the share to the pipeline\'s user: the pinned Quickwit image.')
param rootImage string = ''

// Both jobs: the platform kills a replica after replicaTimeout, which the
// ingest-job-failed alert reports as a failure. Curation stops claiming at a
// max runtime instead, and a batch already downloading then takes at most 45
// minutes more (the per-batch watchdog), so each command ends on its own.
var replicaTimeoutSecs = 86400
// Backfill: 22 h + 45 min watchdog = 22 h 45 min, leaving 75 minutes of
// margin under the 24 h timeout for the startup enqueue and the last commit.
var backfillMaxRuntimeSecs = 79200
// Ingest run: curation stops 6 h after the start (6 h 45 min at most with the
// watchdog). titles-sync takes up to about 4.5 h (a catalog built from
// nothing), and a full rebuild of the corpus about 11 h (23.7M pages at the
// slowest rate of the September 2026 prod releases, 36,000 docs a minute),
// so even all three at their longest end by about 22 h 15 min. The release
// then waits up to 90 minutes for its merges (`--merge-timeout-secs`,
// 08 §8.4): 23 h 45 min.
var ingestCurateMaxRuntimeSecs = 21600

var scratch = !empty(scratchStorageName)
// The pipeline runs as uid 10001 (Dockerfile.ingest); a new NFS share's root
// belongs to root.
var scratchDir = '/scratch/usnm'
// The init container's 0.25 vCPU / 0.5 GiB count toward the replica's
// 4 vCPU / 8 GiB on the Consumption profile.
var ingestResources = scratch ? { cpu: json('3.75'), memory: '7.5Gi' } : { cpu: json('4.0'), memory: '8Gi' }
var scratchEnv = scratch
  ? [
      { name: 'USNM_WORK_DIR', value: scratchDir }
      { name: 'USNM_WORK_MIN_FREE_GIB', value: string(scratchGiB * 3 / 4) }
    ]
  : []

var env = [
  { name: 'USNM_COSMOS_ENDPOINT', value: cosmosEndpoint }
  { name: 'USNM_CURATED_URL', value: '${storageBlobEndpoint}curated' }
  { name: 'USNM_REFERENCE_URL', value: '${storageBlobEndpoint}reference' }
  // Selects id-usnm-ingest at the managed identity endpoint.
  { name: 'AZURE_CLIENT_ID', value: ingestClientId }
  { name: 'RUST_LOG', value: 'info' }
  // Traces and metrics to Application Insights, signed with the identity above.
  { name: 'APPLICATIONINSIGHTS_CONNECTION_STRING', value: appInsightsConnectionString }
]

resource ingest 'Microsoft.App/jobs@2025-01-01' = {
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
      replicaTimeout: replicaTimeoutSecs
      replicaRetryLimit: 0
    }
    template: {
      initContainers: scratch
        ? [
            {
              name: 'scratch-owner'
              image: rootImage
              command: ['/bin/sh']
              args: ['-c', 'mkdir -p ${scratchDir} && chown 10001:10001 ${scratchDir}']
              resources: { cpu: json('0.25'), memory: '0.5Gi' }
              volumeMounts: [{ volumeName: 'scratch', mountPath: '/scratch' }]
            }
          ]
        : null
      containers: [
        {
          name: 'ingest'
          image: image
          args: concat(
            [
              'run'
              '--curate-max-runtime-secs'
              string(ingestCurateMaxRuntimeSecs)
              '--quickwit-bin'
              '/usr/local/bin/quickwit'
              '--quickwit-metastore'
              'azure://qw-index'
              '--quickwit-index-root'
              'azure://qw-index'
            ],
            full ? ['--full'] : []
          )
          resources: ingestResources
          env: concat(env, scratchEnv, [{ name: 'QW_AZURE_STORAGE_ACCOUNT', value: storageAccountName }])
          volumeMounts: scratch ? [{ volumeName: 'scratch', mountPath: '/scratch' }] : null
        }
      ]
      volumes: scratch ? [{ name: 'scratch', storageType: 'NfsAzureFile', storageName: scratchStorageName }] : null
    }
  }
}

resource backfill 'Microsoft.App/jobs@2025-01-01' = {
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
      // From the environment's settings, so a deployment keeps a backfill
      // schedule instead of resetting the job to manual.
      triggerType: empty(backfillCron) ? 'Manual' : 'Schedule'
      manualTriggerConfig: empty(backfillCron) ? { parallelism: workers, replicaCompletionCount: workers } : null
      scheduleTriggerConfig: empty(backfillCron)
        ? null
        : { cronExpression: backfillCron, parallelism: workers, replicaCompletionCount: workers }
      replicaTimeout: replicaTimeoutSecs
      // A worker that dies leaves its batch leased; another replica or run
      // picks it up once the lease expires.
      replicaRetryLimit: 1
    }
    template: {
      containers: [
        {
          name: 'curate'
          image: image
          args: ['curate', '--enqueue', '--max-runtime-secs', string(backfillMaxRuntimeSecs)]
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
