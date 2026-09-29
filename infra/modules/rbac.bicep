// Least-privilege data-plane access for the managed identities (08 §8.2).
// Serving replicas can read reference data and the index and write only the
// response cache; the ingest identity writes the lake and Cosmos state. Both
// send telemetry to Application Insights.

param storageName string
param cosmosName string
param appInsightsName string
param appPrincipalId string
param ingestPrincipalId string

var blobReader = '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'
var blobContributor = 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
// Cosmos DB Built-in Data Contributor (data-plane role).
var cosmosDataContributor = '00000000-0000-0000-0000-000000000002'
// Monitoring Metrics Publisher: sends any telemetry (traces too) to the
// component, which accepts Entra-authenticated ingestion only.
var metricsPublisher = '3913510d-42f4-4e42-8a64-420c390055eb'

var grants = [
  { container: 'reference', principal: appPrincipalId, role: blobReader }
  { container: 'qw-index', principal: appPrincipalId, role: blobReader }
  { container: 'cache', principal: appPrincipalId, role: blobContributor }
  { container: 'curated', principal: ingestPrincipalId, role: blobContributor }
  { container: 'reference', principal: ingestPrincipalId, role: blobContributor }
  { container: 'qw-index', principal: ingestPrincipalId, role: blobContributor }
]

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageName

  resource blobs 'blobServices' existing = {
    name: 'default'
  }
}

resource containers 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' existing = [
  for g in grants: {
    parent: account::blobs
    name: g.container
  }
]

resource blobGrants 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (g, i) in grants: {
    scope: containers[i]
    name: guid(containers[i].id, g.principal, g.role)
    properties: {
      principalId: g.principal
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', g.role)
    }
  }
]

resource cosmos 'Microsoft.DocumentDB/databaseAccounts@2024-11-15' existing = {
  name: cosmosName
}

resource appInsights 'Microsoft.Insights/components@2020-02-02' existing = {
  name: appInsightsName
}

// The ingest and backfill jobs export traces and metrics as id-usnm-ingest,
// the API as id-usnm-app.
resource telemetry 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for principal in [ingestPrincipalId, appPrincipalId]: {
    scope: appInsights
    name: guid(appInsights.id, principal, metricsPublisher)
    properties: {
      principalId: principal
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', metricsPublisher)
    }
  }
]

resource ingestCosmos 'Microsoft.DocumentDB/databaseAccounts/sqlRoleAssignments@2024-11-15' = {
  parent: cosmos
  name: guid(cosmos.id, ingestPrincipalId, cosmosDataContributor)
  properties: {
    principalId: ingestPrincipalId
    roleDefinitionId: '${cosmos.id}/sqlRoleDefinitions/${cosmosDataContributor}'
    scope: '${cosmos.id}/dbs/usnm'
  }
}
