// Experimental Quickwit search cluster (#238, #239; docs/operations.md,
// "Search cluster experiment"). Deployed only with `searchCluster` on; with
// it off main.bicep skips this module, and the stack deletes what it made.
//
// - Nodes: one app per member, `ca-usnm-qw-{i}`, each exactly one replica
//   (min = max = 1) on the Consumption profile. Node 0 is the only metastore
//   (the single writer of the file-backed metastore at `azure://qw-cluster`),
//   the control plane, the janitor, an indexer and a searcher, and the root
//   that searches enter through (internal ingress on 7280, for the bench
//   job). Nodes 1 to `indexers - 1` are indexers and searchers, the rest
//   searchers only. Nothing outside the environment can reach them.
// - Addresses (#239): an app's name resolves to its service IP, which carries
//   only its ingress port, so Quickwit's UDP gossip and gRPC go to replica IPs.
//   `usnm-qwcluster node` (crates/usnm-ingest/src/cluster/node.rs) advertises
//   the replica's IP, writes it to `qw-bench/seeds/qw-{i}.json`, reads the
//   other nodes' entries as peer seeds and then becomes Quickwit (exec). A
//   node that restarts at a new IP registers again and joins through the
//   others; they learn its new address by gossip.
// - Bench job `caj-usnm-qwbench-{env}` (manual): `usnm-qwcluster` builds the
//   1% sample from `curated` (read only), loads it into the cluster, runs the
//   benchmark searches against node 0, and logs and stores its reports in
//   `qw-bench/runs/`.
//
// Storage, Entra only (ADR-0009): two containers of their own in the data
// account; nothing here can write `qw-index`, `reference` or `curated`.
// - `qw-cluster`: the cluster's metastore and splits. Quickwit 0.9 can only
//   use a system-assigned identity in Container Apps (08 §8.2), so each node's
//   system identity gets Blob Data Contributor on this container alone.
// - `qw-bench`: the seed registry, the sample documents and the reports.
//   Blob Data Contributor for `id-usnm-qwnode-{env}` (the nodes' own code:
//   seeds) and `id-usnm-qwbench-{env}` (the bench job), both user-assigned.
// - The bench identity reads `curated` and `reference` (Blob Data Reader).
// Both identities pull the ingest image (AcrPull): it has Quickwit and the
// `usnm-qwcluster` binary.

param location string
param tags object
@description('Environment name, for the job and identity names.')
param nameSuffix string
param environmentId string
@description('The ingest image (Quickwit 0.9.1 with usnm-ingest and usnm-qwcluster).')
param image string
param registryServer string
param registryName string
param storageAccountName string
param storageBlobEndpoint string
@description('Cluster members (apps ca-usnm-qw-0 to -{nodes - 1}). 0 runs no node and keeps the containers and the bench job: the idle state between runs.')
@minValue(0)
@maxValue(4)
param nodes int = 1
@description('How many of the nodes, from node 0, run an indexer. The rest search only.')
@minValue(1)
@maxValue(4)
param indexers int = 1
@description('vCPU per node, with 2 GiB of memory per vCPU (the Consumption profile\'s ratio). 2 is the API sidecar\'s size.')
@minValue(1)
@maxValue(4)
param nodeVcpu int = 2

var clusterContainer = 'qw-cluster'
var benchContainer = 'qw-bench'
// What node 0 adds to the indexer and searcher every node may run.
var rootServices = ['metastore', 'control_plane', 'janitor']
// Inside the environment: node 0's internal ingress, by app name.
var rootUrl = 'http://ca-usnm-qw-0'

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageAccountName

  resource blobs 'blobServices' existing = {
    name: 'default'
  }
}

resource cluster 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: account::blobs
  name: clusterContainer
  properties: { publicAccess: 'None' }
}

resource bench 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: account::blobs
  name: benchContainer
  properties: { publicAccess: 'None' }
}

resource curated 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' existing = {
  parent: account::blobs
  name: 'curated'
}

resource reference 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' existing = {
  parent: account::blobs
  name: 'reference'
}

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' existing = {
  name: registryName
}

resource nodeIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: 'id-usnm-qwnode-${nameSuffix}'
  location: location
  tags: tags
}

resource benchIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: 'id-usnm-qwbench-${nameSuffix}'
  location: location
  tags: tags
}

var blobReader = '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'
var blobContributor = 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
var acrPull = '7f951dda-4ed3-4680-a7ca-43fe172d538d'

// Both identities, by position: 0 the nodes', 1 the bench job's.
var identityIds = [nodeIdentity.id, benchIdentity.id]

resource pulls 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (id, i) in identityIds: {
    scope: registry
    name: guid(registry.id, id, acrPull)
    properties: {
      principalId: i == 0 ? nodeIdentity.properties.principalId : benchIdentity.properties.principalId
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPull)
    }
  }
]

resource benchWriters 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (id, i) in identityIds: {
    scope: bench
    name: guid(bench.id, id, blobContributor)
    properties: {
      principalId: i == 0 ? nodeIdentity.properties.principalId : benchIdentity.properties.principalId
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobContributor)
    }
  }
]

resource curatedReader 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: curated
  name: guid(curated.id, benchIdentity.id, blobReader)
  properties: {
    principalId: benchIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobReader)
  }
}

resource referenceReader 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: reference
  name: guid(reference.id, benchIdentity.id, blobReader)
  properties: {
    principalId: benchIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobReader)
  }
}

// At most 4 nodes, so `indexers` past `nodes` just means all of them.
var nodeCpu = json(string(nodeVcpu))
var nodeMemory = '${nodeVcpu * 2}Gi'

resource nodeApps 'Microsoft.App/containerApps@2024-03-01' = [
  for i in range(0, nodes): {
    name: 'ca-usnm-qw-${i}'
    location: location
    tags: tags
    // The user-assigned identity pulls the image and writes the seed entry;
    // Quickwit uses the system-assigned one (see above).
    dependsOn: [pulls, benchWriters]
    identity: {
      type: 'SystemAssigned,UserAssigned'
      userAssignedIdentities: { '${nodeIdentity.id}': {} }
    }
    properties: {
      environmentId: environmentId
      workloadProfileName: 'Consumption'
      configuration: {
        activeRevisionsMode: 'Single'
        registries: [{ server: registryServer, identity: nodeIdentity.id }]
        // Node 0 only: the root, for the bench job inside the environment.
        // Plain HTTP on the environment's internal network; nothing outside
        // it can connect.
        ingress: i == 0
          ? {
              external: false
              targetPort: 7280
              transport: 'http'
              allowInsecure: true
            }
          : null
      }
      template: {
        containers: [
          {
            name: 'qwnode'
            image: image
            command: ['/usr/local/bin/usnm-qwcluster']
            args: [
              'node'
              '--node-id'
              'qw-${i}'
              '--services'
              join(
                concat(i == 0 ? rootServices : [], i < indexers ? ['indexer'] : [], ['searcher']),
                ','
              )
              '--registry'
              '${storageBlobEndpoint}${benchContainer}'
              '--metastore'
              'azure://${clusterContainer}'
              '--index-root'
              'azure://${clusterContainer}'
              '--storage-account'
              storageAccountName
              '--cpus'
              string(nodeVcpu)
            ]
            resources: { cpu: nodeCpu, memory: nodeMemory }
            env: [
              // Selects id-usnm-qwnode for the seed registry; the wrapper
              // removes it before Quickwit starts.
              { name: 'AZURE_CLIENT_ID', value: nodeIdentity.properties.clientId }
              // As the API's sidecar: no search text in the logs (09 §9.4.2).
              {
                name: 'RUST_LOG'
                value: 'info,quickwit_serve::search_api=warn,quickwit_search=warn,tantivy=warn'
              }
            ]
            probes: [
              {
                type: 'Startup'
                httpGet: { path: '/health/livez', port: 7280 }
                periodSeconds: 5
                failureThreshold: 60
              }
              {
                type: 'Liveness'
                httpGet: { path: '/health/livez', port: 7280 }
                periodSeconds: 30
              }
            ]
          }
        ]
        scale: { minReplicas: 1, maxReplicas: 1 }
      }
    }
  }
]

// Splits and the metastore: each node's system identity (Quickwit's).
resource clusterWriters 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for i in range(0, nodes): {
    scope: cluster
    name: guid(cluster.id, nodeApps[i].id, blobContributor)
    properties: {
      principalId: nodeApps[i].identity.principalId
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobContributor)
    }
  }
]

resource benchJob 'Microsoft.App/jobs@2025-01-01' = {
  name: 'caj-usnm-qwbench-${nameSuffix}'
  location: location
  tags: tags
  dependsOn: [pulls, benchWriters, curatedReader, referenceReader]
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: { '${benchIdentity.id}': {} }
  }
  properties: {
    environmentId: environmentId
    workloadProfileName: 'Consumption'
    configuration: {
      registries: [{ server: registryServer, identity: benchIdentity.id }]
      triggerType: 'Manual'
      manualTriggerConfig: { parallelism: 1, replicaCompletionCount: 1 }
      // The sample reads the whole curated lake once (about 1 to 2 h).
      replicaTimeout: 21600
      replicaRetryLimit: 0
    }
    template: {
      containers: [
        {
          name: 'qwbench'
          image: image
          command: ['/usr/local/bin/usnm-qwcluster']
          // scripts/qwcluster.sh starts it with each step's arguments.
          args: ['members']
          resources: { cpu: json('4.0'), memory: '8Gi' }
          env: [
            { name: 'AZURE_CLIENT_ID', value: benchIdentity.properties.clientId }
            { name: 'USNM_CURATED_URL', value: '${storageBlobEndpoint}curated' }
            { name: 'USNM_REFERENCE_URL', value: '${storageBlobEndpoint}reference' }
            { name: 'USNM_QWBENCH_URL', value: '${storageBlobEndpoint}${benchContainer}' }
            { name: 'USNM_QWCLUSTER_URL', value: rootUrl }
            { name: 'USNM_QWCLUSTER_INDEX_ROOT', value: 'azure://${clusterContainer}' }
            { name: 'RUST_LOG', value: 'info' }
          ]
        }
      ]
    }
  }
}

output nodeApps array = [for i in range(0, nodes): nodeApps[i].name]
output benchJobName string = benchJob.name
output rootUrl string = rootUrl
