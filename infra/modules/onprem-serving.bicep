// The on-prem serving container's access (#955, Phase 2): the API and site
// served from a container on-prem, as that container's Arc machine
// identity. Its own identity, not the on-prem hosts' group: the group holds
// other hosts, and serving faces the internet.
//
// What id-usnm-app has (rbac.bicep), less the lake's qw-index, since the
// container serves its index from local disk:
//
// - reference: Storage Blob Data Reader;
// - cache: Storage Blob Data Contributor;
// - searches and ocr-requests: the search log writer role (list, read,
//   create; no delete), which rbac.bicep defines; the API writes each blob
//   create-only (If-None-Match: *);
// - Cosmos: Data Reader on batches, index_runs and ops only, for
//   /v1/status and /v1/versions;
// - Application Insights: Monitoring Metrics Publisher (Entra-only ingestion);
// - the registry: AcrPull, for the API image by digest.

param storageName string
param cosmosName string
param appInsightsName string
param registryName string
@description('Object id of the serving container\'s Arc machine identity.')
param principalId string
@description('The search log writer role\'s name (a GUID), from rbac.bicep.')
param searchLogRoleName string

var blobReader = '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'
var blobContributor = 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
var cosmosDataReader = '00000000-0000-0000-0000-000000000001'
var metricsPublisher = '3913510d-42f4-4e42-8a64-420c390055eb'
var acrPull = '7f951dda-4ed3-4680-a7ca-43fe172d538d'

resource lake 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageName

  resource blobService 'blobServices' existing = {
    name: 'default'

    resource reference 'containers' existing = {
      name: 'reference'
    }

    resource cache 'containers' existing = {
      name: 'cache'
    }

    resource searches 'containers' existing = {
      name: 'searches'
    }

    resource ocrRequests 'containers' existing = {
      name: 'ocr-requests'
    }
  }
}

resource cosmos 'Microsoft.DocumentDB/databaseAccounts@2024-11-15' existing = {
  name: cosmosName
}

resource appInsights 'Microsoft.Insights/components@2020-02-02' existing = {
  name: appInsightsName
}

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' existing = {
  name: registryName
}

resource referenceRead 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::reference
  name: guid(lake::blobService::reference.id, principalId, blobReader)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobReader)
    description: 'On-prem serving (#955): reads reference data.'
  }
}

resource cacheWrite 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::cache
  name: guid(lake::blobService::cache.id, principalId, blobContributor)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', blobContributor)
    description: 'On-prem serving (#955): the response cache.'
  }
}

resource searchLogWrite 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::searches
  name: guid(lake::blobService::searches.id, principalId, 'usnm-search-log-writer')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    // Custom roles are addressed at subscription level wherever they're defined.
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', searchLogRoleName)
    description: 'On-prem serving (#955): adds to the search log; no delete.'
  }
}

resource ocrRequestsWrite 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: lake::blobService::ocrRequests
  name: guid(lake::blobService::ocrRequests.id, principalId, 'usnm-search-log-writer')
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', searchLogRoleName)
    description: 'On-prem serving (#955): adds OCR requests; no delete.'
  }
}

// The same containers as id-usnm-app's grant (rbac.bicep), read-only.
var statusContainers = ['batches', 'index_runs', 'ops']

resource statusRead 'Microsoft.DocumentDB/databaseAccounts/sqlRoleAssignments@2024-11-15' = [
  for c in statusContainers: {
    parent: cosmos
    name: guid(cosmos.id, principalId, cosmosDataReader, c)
    properties: {
      principalId: principalId
      roleDefinitionId: '${cosmos.id}/sqlRoleDefinitions/${cosmosDataReader}'
      scope: '${cosmos.id}/dbs/usnm/colls/${c}'
    }
  }
]

resource telemetry 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: appInsights
  name: guid(appInsights.id, principalId, metricsPublisher)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', metricsPublisher)
    description: 'On-prem serving (#955): sends the API\'s telemetry.'
  }
}

resource pull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: registry
  name: guid(registry.id, principalId, acrPull)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPull)
    description: 'On-prem serving (#955): pulls the API image by digest.'
  }
}
