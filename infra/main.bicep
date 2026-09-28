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

@description('Region; East US 2 has ACI Spot (preview).')
param location string = 'eastus2'

@description('A public API image, used only while useAcr is off. Empty (the default) skips the API until CI has pushed to the registry and useAcr is on.')
param apiImage string = ''

@description('Extra origins allowed by API CORS and the tiles account. The app\'s own hostname and, with dnsZoneName, the domain and its www are always allowed.')
param allowedOrigins array = []

@description('0 lets dev scale to zero; production keeps one warm replica.')
param apiMinReplicas int = toLower(environmentName) == 'prod' ? 1 : 0

@description('fixtures: the API serves its baked synthetic data. quickwit: reference data from Blob and a read-only Quickwit sidecar over qw-index (needs published indexes).')
@allowed(['fixtures', 'quickwit'])
param searchBackend string = 'fixtures'

@description('Pull images from the private registry (08 §8.6). Turn on once CI has pushed them there.')
param useAcr bool = false

@description('Image tag CI pushed to the registry (main or a commit sha).')
param imageTag string = 'main'

@description('GitHub repository that deploys this environment, as owner/name. Its GitHub Environment named after this azd environment may use the CI identity.')
param githubRepo string = 'tgoodyear/usnewsmap.com'

@description('The same repository as `owner@ownerId/name@repoId` (GitHub\'s immutable-ID OIDC subject format); scripts/bootstrap.sh looks it up.')
param githubRepoIds string = 'tgoodyear@116683/usnewsmap.com@1389862972'

@description('Create the ingest and backfill jobs (needs useAcr: the ingest image is only in the private registry).')
param ingestJobs bool = false

@description('Weekly ingest schedule, UTC cron (e.g. "17 3 * * 1"). Empty: run the job manually.')
param ingestCron string = ''

@description('Parallel curation workers in the backfill job.')
param backfillWorkers int = 8

@description('Cosmos DB free tier (one per subscription). False makes the account serverless.')
param cosmosFreeTier bool = true

@description('Comma-separated alert and budget recipients. Budget and alerts are skipped when empty.')
param alertEmails string = ''


@description('Budget start month, YYYY-MM-01. Set once per environment (a budget\'s start date cannot change); the budget is skipped when empty.')
param budgetStartDate string = ''


@description('Create and assign the guardrail policies (needs Resource Policy Contributor on the subscription).')
param deployPolicies bool = true

@description('Public DNS zone for the site (e.g. usnewsmap.com); empty skips it. Delegate the domain to the NAME_SERVERS output.')
param dnsZoneName string = ''

@description('Managed certificate for api.{dnsZoneName}, recorded by scripts/bootstrap.sh after it issues one.')
param apiCertificateId string = ''

@description('Managed certificate for {dnsZoneName} itself (the site), recorded by scripts/bootstrap.sh.')
param siteCertificateId string = ''

@description('Managed certificate for www.{dnsZoneName}, recorded by scripts/bootstrap.sh.')
param wwwCertificateId string = ''

// Resource names must be lowercase (Cosmos, storage).
var env = toLower(environmentName)
var tags = {
  project: 'usnewsmap'
  environment: environmentName
  'azd-env-name': environmentName
}
var emails = filter(map(split(alertEmails, ','), e => trim(e)), e => !empty(e))
var suffix = take(uniqueString(subscription().id, env), 6)
// Quickwit v0.9.1, copied into the registry by CI with its digest unchanged.
var quickwitDigest = 'sha256:3e0f079eb57dd5563f36a457e9a7a2963ff882316d6c77e3180ac3c59767a68f'
// Where the site is served: the API app's own hostname, and the domain and
// www if there is one. The site calls the API on its own origin, so these
// matter for the tiles account's CORS (and any cross-origin API caller).
var appName = 'ca-usnm-${env}'
var appOrigin = 'https://${appName}.${containerEnv.outputs.defaultDomain}'
var domainOrigins = empty(dnsZoneName) ? [] : ['https://${dnsZoneName}', 'https://www.${dnsZoneName}']
var siteOrigins = union(allowedOrigins, domainOrigins, [appOrigin])
// Hostnames on the app, each bound once bootstrap has issued its certificate.
var customDomains = empty(dnsZoneName)
  ? []
  : [
      { name: dnsZoneName, certificateId: siteCertificateId }
      { name: 'www.${dnsZoneName}', certificateId: wwwCertificateId }
      { name: 'api.${dnsZoneName}', certificateId: apiCertificateId }
    ]

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

module registry 'modules/registry.bicep' = {
  scope: rg
  name: 'registry'
  params: {
    location: location
    tags: tags
    name: 'crusnm${env}${suffix}'
    ciIdentityName: 'id-usnm-ci-${env}'
    githubRepo: githubRepo
    githubRepoIds: githubRepoIds
    githubEnvironment: env
    pullPrincipalIds: [identities.outputs.appPrincipalId, identities.outputs.ingestPrincipalId]
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
    allowedOrigins: siteOrigins
  }
}

module cosmos 'modules/cosmos.bicep' = {
  scope: rg
  name: 'cosmos'
  params: {
    location: location
    tags: tags
    name: 'cosmos-usnm-${env}-${suffix}'
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
  }
}

// Resource logs for everything that has them, to the one workspace.
module diagnostics 'modules/diagnostics.bicep' = {
  scope: rg
  name: 'diagnostics'
  params: {
    workspaceId: monitoring.outputs.workspaceId
    workspaceName: monitoring.outputs.workspaceName
    registryName: registry.outputs.name
    vnetName: network.outputs.vnetName
    containerEnvName: containerEnv.outputs.name
    cosmosName: cosmos.outputs.name
    dataStorageName: storage.outputs.name
    tilesStorageName: tiles.outputs.name
  }
}

// A new environment has no image to pull until CI pushes to its registry.
var deployApi = useAcr || !empty(apiImage)

module api 'modules/containerapp.bicep' = if (deployApi) {
  scope: rg
  name: 'api'
  dependsOn: [rbac, privateEndpoints]
  params: {
    location: location
    tags: tags
    name: appName
    environmentId: containerEnv.outputs.id
    image: useAcr ? '${registry.outputs.loginServer}/usnewsmap-api:${imageTag}' : apiImage
    quickwitImage: useAcr
      ? '${registry.outputs.loginServer}/quickwit/quickwit@${quickwitDigest}'
      : 'quickwit/quickwit:v0.9.1@${quickwitDigest}'
    registryServer: useAcr ? registry.outputs.loginServer : ''
    customDomains: customDomains
    identityId: identities.outputs.appId
    identityClientId: identities.outputs.appClientId
    storageBlobEndpoint: storage.outputs.blobEndpoint
    storageAccountName: storage.outputs.name
    allowedOrigins: siteOrigins
    minReplicas: apiMinReplicas
    searchBackend: searchBackend
  }
}

// What CI may do here: roll the API app (and so the site) onto new images.
module deployer 'modules/deployer.bicep' = {
  scope: rg
  name: 'deployer'
  params: {
    principalId: registry.outputs.ciPrincipalId
    appIdentityName: identities.outputs.appName
  }
}

module ingest 'modules/ingestjobs.bicep' = if (ingestJobs && useAcr) {
  scope: rg
  name: 'ingest-jobs'
  dependsOn: [rbac, privateEndpoints]
  params: {
    location: location
    tags: tags
    environmentId: containerEnv.outputs.id
    image: '${registry.outputs.loginServer}/usnewsmap-ingest:${imageTag}'
    registryServer: registry.outputs.loginServer
    ingestIdentityId: identities.outputs.ingestId
    ingestClientId: identities.outputs.ingestClientId
    storageAccountName: storage.outputs.name
    storageBlobEndpoint: storage.outputs.blobEndpoint
    cosmosEndpoint: cosmos.outputs.endpoint
    jobNameSuffix: env
    cron: ingestCron
    workers: backfillWorkers
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
  // Subscription-scope deployment: named per environment so that two
  // environments in one subscription don't overwrite each other's history.
  name: 'usnm-policy-definitions-${env}'
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

module dns 'modules/dns.bicep' = if (!empty(dnsZoneName)) {
  scope: rg
  name: 'dns'
  params: {
    tags: tags
    zoneName: dnsZoneName
    appFqdn: deployApi ? api!.outputs.fqdn : ''
    staticIp: containerEnv.outputs.staticIp
    verificationId: containerEnv.outputs.customDomainVerificationId
  }
}

output AZURE_LOCATION string = location
output AZURE_RESOURCE_GROUP string = rg.name
// The custom hostname once its certificate is bound, else the app's own.
output API_URL string = !deployApi
  ? ''
  : (!empty(dnsZoneName) && !empty(apiCertificateId) ? 'https://api.${dnsZoneName}' : 'https://${api!.outputs.fqdn}')
output API_APP string = deployApi ? api!.outputs.name : ''
// The domain once its certificate is bound, else the app's own hostname.
output SITE_URL string = !deployApi
  ? ''
  : (!empty(dnsZoneName) && !empty(siteCertificateId) ? 'https://${dnsZoneName}' : 'https://${api!.outputs.fqdn}')
output CONTAINER_ENV_NAME string = 'cae-usnm-${env}'
// Set these as the domain's name servers at the registrar.
output NAME_SERVERS string = empty(dnsZoneName) ? '' : join(dns!.outputs.nameServers, ' ')
output TILES_URL string = tiles.outputs.tilesUrl
output STORAGE_ACCOUNT string = storage.outputs.name
output COSMOS_ENDPOINT string = cosmos.outputs.endpoint
output INGEST_JOB string = ingestJobs && useAcr ? ingest!.outputs.ingestJobName : ''
output BACKFILL_JOB string = ingestJobs && useAcr ? ingest!.outputs.backfillJobName : ''
output ACR_NAME string = registry.outputs.name
output ACR_LOGIN_SERVER string = registry.outputs.loginServer
// For the GitHub Environment variables CI signs in with (not secrets; scripts/bootstrap.sh writes them).
output CI_CLIENT_ID string = registry.outputs.ciClientId
output AZURE_TENANT_ID string = tenant().tenantId
output AZURE_SUBSCRIPTION_ID string = subscription().subscriptionId
output APPLICATIONINSIGHTS_CONNECTION_STRING string = monitoring.outputs.appInsightsConnectionString
