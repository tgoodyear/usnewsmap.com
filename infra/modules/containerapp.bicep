// The public API and the web app it serves (08 §8.1, ADR-0009).
//
// searchBackend = 'fixtures': the API serves its baked synthetic fixtures
// (memory backend) while exercising the production paths: managed identity,
// the Blob private endpoint (persistent response cache), ingress, probes and
// scaling.
//
// searchBackend = 'quickwit': reference data comes from `reference/` and a
// read-only Quickwit searcher sidecar serves `qw-index/` on localhost (08
// §8.4.1). Quickwit 0.9 authenticates to Blob with Azure's default credential
// chain, which in Container Apps can only use the app's *system-assigned*
// identity, so the app gets one with Blob Data Reader on `qw-index` only
// (spike S-2). Needs indexes and a current.json published by the ingest jobs.

param location string
param tags object
param name string
param environmentId string
param image string
param identityId string
param identityClientId string
param storageBlobEndpoint string
param allowedOrigins array
param storageAccountName string
param minReplicas int
param maxReplicas int = 2
@description('Proxies appending to X-Forwarded-For: 1 = Container Apps ingress only.')
param trustedProxyHops int = 1
@allowed(['fixtures', 'quickwit'])
param searchBackend string = 'fixtures'
@description('Quickwit image, pinned by digest (v0.9.1; the version spike S-2 validated).')
param quickwitImage string = 'quickwit/quickwit:v0.9.1@sha256:3e0f079eb57dd5563f36a457e9a7a2963ff882316d6c77e3180ac3c59767a68f'
@description('Private registry the images come from (pulled with the app identity); empty for a public registry.')
param registryServer string = ''
@description('Application Insights connection string (names the ingestion endpoint; not a credential). Empty: the API exports no telemetry.')
param appInsightsConnectionString string = ''
@description('Custom hostnames with their managed certificates, as { name, certificateId }. scripts/bootstrap.sh issues each certificate once DNS is delegated; a name with no certificate yet is left out. Declaring them here keeps a re-deploy from dropping the bindings.')
param customDomains array = []

var quickwit = searchBackend == 'quickwit'

// Searcher-only node on localhost. The metastore service opens the
// file-backed metastore read-only and polls it (08 §8.4.1); the API looks
// each index up before serving a version that lists it, because the
// searcher reads the index list only at start (S-2). Aggregation limits per
// 05 §5.7; caches sized for the 2 GiB container.
var quickwitConfig = join([
  'version: 0.8'
  'cluster_id: usnm-searcher'
  'node_id: searcher'
  'enabled_services: [searcher, metastore]'
  'listen_address: 127.0.0.1'
  'rest:'
  '  listen_port: 7280'
  'data_dir: /quickwit/qwdata'
  'metastore_uri: azure://qw-index#polling_interval=30s'
  'default_index_root_uri: azure://qw-index'
  'storage:'
  '  azure:'
  '    account: ${storageAccountName}'
  'searcher:'
  '  aggregation_bucket_limit: 200000'
  '  aggregation_memory_limit: 768MB'
  '  fast_field_cache_capacity: 384MB'
  '  split_footer_cache_capacity: 128MB'
  '  partial_request_cache_capacity: 32MB'
  '  predicate_cache_capacity: 32MB'
  '  max_num_concurrent_split_searches: 8'
], '\n')

var apiEnv = [
  { name: 'USNM_RESPONSE_CACHE_URL', value: '${storageBlobEndpoint}cache' }
  { name: 'USNM_ALLOWED_ORIGINS', value: join(allowedOrigins, ',') }
  { name: 'USNM_TRUSTED_PROXY_HOPS', value: string(trustedProxyHops) }
  // Selects the user-assigned identity at the managed identity endpoint.
  { name: 'AZURE_CLIENT_ID', value: identityClientId }
]
// Requests, traces and metrics go to Application Insights as id-usnm-app,
// with an Entra token (08 §8.1.2).
var telemetryEnv = empty(appInsightsConnectionString)
  ? []
  : [{ name: 'APPLICATIONINSIGHTS_CONNECTION_STRING', value: appInsightsConnectionString }]
var backendEnv = quickwit
  ? [
      { name: 'USNM_BACKEND', value: 'quickwit' }
      { name: 'USNM_QUICKWIT_URL', value: 'http://127.0.0.1:7280' }
      { name: 'USNM_REFERENCE_URL', value: '${storageBlobEndpoint}reference' }
    ]
  : [{ name: 'USNM_BACKEND', value: 'memory' }]

var apiContainer = {
  name: 'api'
  image: image
  resources: { cpu: json('0.25'), memory: '0.5Gi' }
  env: concat(backendEnv, apiEnv, telemetryEnv)
  probes: [
    {
      type: 'Liveness'
      httpGet: { path: '/healthz', port: 8080 }
      periodSeconds: 30
    }
    {
      type: 'Readiness'
      httpGet: { path: '/readyz', port: 8080 }
      periodSeconds: 10
    }
  ]
}

var quickwitContainer = {
  name: 'quickwit'
  image: quickwitImage
  // The image has no config for this role; write it from the environment.
  command: ['/bin/sh', '-c', 'printf \'%s\\n\' "$USNM_QW_CONFIG" > /tmp/node.yaml && exec quickwit run --config /tmp/node.yaml']
  resources: { cpu: json('1.0'), memory: '2Gi' }
  env: [
    { name: 'USNM_QW_CONFIG', value: quickwitConfig }
    { name: 'QW_DISABLE_TELEMETRY', value: '1' }
    // The image sets QW_LISTEN_ADDRESS=0.0.0.0, which overrides the config's
    // listen_address, and Quickwit refuses 0.0.0.0 without an advertise
    // address ("listen address `0.0.0.0` is unspecified"). Pin it here.
    { name: 'QW_LISTEN_ADDRESS', value: '127.0.0.1' }
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

resource app 'Microsoft.App/containerApps@2024-03-01' = {
  name: name
  location: location
  tags: tags
  identity: {
    // Quickwit can use only the system-assigned identity (see above).
    type: quickwit ? 'SystemAssigned,UserAssigned' : 'UserAssigned'
    userAssignedIdentities: { '${identityId}': {} }
  }
  properties: {
    environmentId: environmentId
    workloadProfileName: 'Consumption'
    configuration: {
      activeRevisionsMode: 'Single'
      registries: empty(registryServer) ? [] : [{ server: registryServer, identity: identityId }]
      ingress: {
        external: true
        targetPort: 8080
        transport: 'http'
        allowInsecure: false
        customDomains: [
          for d in filter(customDomains, d => !empty(d.certificateId)): {
            name: d.name
            certificateId: d.certificateId
            bindingType: 'SniEnabled'
          }
        ]
      }
    }
    template: {
      containers: quickwit ? [apiContainer, quickwitContainer] : [apiContainer]
      scale: {
        minReplicas: minReplicas
        maxReplicas: maxReplicas
        rules: [
          {
            name: 'http'
            http: { metadata: { concurrentRequests: '50' } }
          }
        ]
      }
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

var blobReader = '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'

// Read-only, and only the index: the sidecar can't write splits or the
// metastore, and can't read anything else.
resource quickwitIndexReader 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (quickwit) {
  scope: account::blobs::index
  name: guid(account::blobs::index.id, app.id, blobReader)
  properties: {
    principalId: app.identity.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobReader)
  }
}

output name string = app.name
output fqdn string = app.properties.configuration.ingress.fqdn
