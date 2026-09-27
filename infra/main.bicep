// US News Map: lean hosting profile (08 §8.1, ADR-0006). No IaaS.
//
// Provisioned with `azd provision` (see azure.yaml). Creates the project
// resource group and the empty Spot resource group the backfill launcher
// will use, then the platform inside the project group.

targetScope = 'subscription'

@minLength(1)
@maxLength(16)
@description('Environment name, e.g. dev or prod (azd sets AZURE_ENV_NAME).')
param environmentName string

@description('Region; East US 2 has ACI Spot (preview) and SWA.')
param location string = 'eastus2'

@description('API container image (public GHCR image; no registry resource).')
param apiImage string = 'ghcr.io/tgoodyear/usnewsmap-api:main'

@description('Origins allowed by API CORS and the tiles account.')
param allowedOrigins array = ['https://usnewsmap.com']

@description('0 lets dev scale to zero; production keeps one warm replica.')
param apiMinReplicas int = toLower(environmentName) == 'prod' ? 1 : 0

@description('Cosmos DB free tier (one per subscription). False makes the account serverless.')
param cosmosFreeTier bool = true

@description('Comma-separated alert and budget recipients. Budget and alerts are skipped when empty.')
param alertEmails string = ''


@description('Budget start month, YYYY-MM-01. Set once per environment (a budget\'s start date cannot change); the budget is skipped when empty.')
param budgetStartDate string = ''


@description('Create and assign the guardrail policies (needs Resource Policy Contributor on the subscription).')
param deployPolicies bool = true

// Resource names must be lowercase (Cosmos, storage).
var env = toLower(environmentName)
var tags = {
  project: 'usnewsmap'
  environment: environmentName
  'azd-env-name': environmentName
}
var emails = filter(map(split(alertEmails, ','), e => trim(e)), e => !empty(e))
var suffix = take(uniqueString(subscription().id, env), 6)

resource rg 'Microsoft.Resources/resourceGroups@2024-03-01' = {
  name: 'rg-usnm-${env}'
  location: location
  tags: tags
}

resource spotRg 'Microsoft.Resources/resourceGroups@2024-03-01' = {
  name: 'rg-usnm-${env}-spot'
  location: location
  tags: tags
}

module monitoring 'modules/monitoring.bicep' = {
  scope: rg
  name: 'monitoring'
  params: {
    location: location
    tags: tags
    workspaceName: 'log-usnm-${env}'
    appInsightsName: 'appi-usnm-${env}'
    actionGroupName: 'ag-usnm-${env}'
    alertEmails: emails
  }
}

module network 'modules/network.bicep' = {
  scope: rg
  name: 'network'
  params: {
    location: location
    tags: tags
    name: 'vnet-usnm-${env}'
  }
}

module identities 'modules/identities.bicep' = {
  scope: rg
  name: 'identities'
  params: {
    location: location
    tags: tags
    appName: 'id-usnm-app-${env}'
    ingestName: 'id-usnm-ingest-${env}'
  }
}

module storage 'modules/storage.bicep' = {
  scope: rg
  name: 'storage'
  params: {
    location: location
    tags: tags
    name: 'stusnmd${suffix}'
    workspaceId: monitoring.outputs.workspaceId
  }
}

module tiles 'modules/tiles.bicep' = {
  scope: rg
  name: 'tiles'
  params: {
    location: location
    // Exempt from the "data services private" audit: public map data only.
    tags: union(tags, { 'usnm-public': 'true' })
    name: 'stusnmt${suffix}'
    allowedOrigins: allowedOrigins
  }
}

module cosmos 'modules/cosmos.bicep' = {
  scope: rg
  name: 'cosmos'
  params: {
    location: location
    tags: tags
    name: 'cosmos-usnm-${env}-${suffix}'
    workspaceId: monitoring.outputs.workspaceId
    freeTier: cosmosFreeTier
  }
}

module privateEndpoints 'modules/private-endpoints.bicep' = {
  scope: rg
  name: 'private-endpoints'
  params: {
    location: location
    tags: tags
    subnetId: network.outputs.peSubnetId
    storageId: storage.outputs.id
    cosmosId: cosmos.outputs.id
    blobDnsZoneId: network.outputs.blobDnsZoneId
    cosmosDnsZoneId: network.outputs.cosmosDnsZoneId
  }
}

module rbac 'modules/rbac.bicep' = {
  scope: rg
  name: 'rbac'
  params: {
    storageName: storage.outputs.name
    cosmosName: cosmos.outputs.name
    appPrincipalId: identities.outputs.appPrincipalId
    ingestPrincipalId: identities.outputs.ingestPrincipalId
  }
}

module containerEnv 'modules/containerapps-env.bicep' = {
  scope: rg
  name: 'containerapps-env'
  params: {
    location: location
    tags: tags
    name: 'cae-usnm-${env}'
    subnetId: network.outputs.caeSubnetId
    workspaceName: monitoring.outputs.workspaceName
  }
}

module api 'modules/containerapp.bicep' = {
  scope: rg
  name: 'api'
  dependsOn: [rbac, privateEndpoints]
  params: {
    location: location
    tags: tags
    name: 'ca-usnm-${env}'
    environmentId: containerEnv.outputs.id
    image: apiImage
    identityId: identities.outputs.appId
    identityClientId: identities.outputs.appClientId
    storageBlobEndpoint: storage.outputs.blobEndpoint
    allowedOrigins: allowedOrigins
    minReplicas: apiMinReplicas
  }
}

module site 'modules/staticwebapp.bicep' = {
  scope: rg
  name: 'staticwebapp'
  params: {
    location: location
    tags: tags
    name: 'swa-usnm-${env}'
  }
}

module budget 'modules/budget.bicep' = if (!empty(emails) && !empty(budgetStartDate)) {
  scope: rg
  name: 'budget'
  params: {
    name: 'budget-usnm-${env}'
    startDate: budgetStartDate
    contactEmails: emails
  }
}

module alerts 'modules/alerts.bicep' = if (!empty(emails)) {
  scope: rg
  name: 'alerts'
  params: {
    name: 'alert-usnm-data-account-change-${env}'
    actionGroupId: monitoring.outputs.actionGroupId
    storageId: storage.outputs.id
    cosmosId: cosmos.outputs.id
  }
}

module policyDefinitions 'modules/policy-definitions.bicep' = if (deployPolicies) {
  name: 'usnm-policy-definitions'
}

module policies 'modules/policy-assignments.bicep' = if (deployPolicies) {
  scope: rg
  name: 'policy-assignments'
  params: {
    definitionIds: policyDefinitions!.outputs.ids
  }
}

module spotPolicies 'modules/policy-assignments.bicep' = if (deployPolicies) {
  scope: spotRg
  name: 'policy-assignments-spot'
  params: {
    definitionIds: policyDefinitions!.outputs.ids
  }
}

output AZURE_LOCATION string = location
output AZURE_RESOURCE_GROUP string = rg.name
output API_URL string = 'https://${api.outputs.fqdn}'
output SITE_URL string = 'https://${site.outputs.defaultHostname}'
output TILES_URL string = tiles.outputs.tilesUrl
output STORAGE_ACCOUNT string = storage.outputs.name
output COSMOS_ENDPOINT string = cosmos.outputs.endpoint
output APPLICATIONINSIGHTS_CONNECTION_STRING string = monitoring.outputs.appInsightsConnectionString
