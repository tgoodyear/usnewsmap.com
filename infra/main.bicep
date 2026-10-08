// US News Map: lean hosting profile (08 §8.1, ADR-0006). No IaaS.
//
// Deployed as the deployment stack usnm-{env} by scripts/bootstrap.sh, with
// parameters from infra/main.bicepparam. Creates the project
// resource group and the empty Spot resource group the backfill launcher
// will use, then the platform inside the project group.

targetScope = 'subscription'

import { guardrailPolicyNames } from 'guardrails.bicep'

@minLength(1)
@maxLength(16)
@description('Environment name, e.g. dev or prod (AZURE_ENV_NAME in the environment\'s settings).')
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

@description('GitHub repository that deploys this environment, as owner/name. Its GitHub Environment of the same name may use the CI identity.')
param githubRepo string = 'tgoodyear/usnewsmap.com'

@description('The same repository as `owner@ownerId/name@repoId` (GitHub\'s immutable-ID OIDC subject format); scripts/bootstrap.sh looks it up.')
param githubRepoIds string = 'tgoodyear@116683/usnewsmap.com@1389862972'

@description('Create the ingest and backfill jobs (needs useAcr: the ingest image is only in the private registry).')
param ingestJobs bool = false

@description('Weekly ingest schedule, UTC cron (e.g. "17 3 * * 1"). Empty: run the job manually.')
param ingestCron string = ''

@description('Parallel curation workers in the backfill job.')
param backfillWorkers int = 4

@description('Make every ingest run a full release (a new, merged base). For a one-off rebuild; clear it afterwards.')
param ingestFull bool = false

@description('Index American Stories\' text with LoC\'s (#218): the first ingest run with it builds a full base. Keep it on afterwards; turning it off makes the next release publish a version whose searches leave the text out (docs/operations.md, "American Stories\' text").')
param americanStories bool = false

@description('Add a memory-optimized E4 workload profile (4 vCPU / 32 GiB, no minimum nodes) for full index rebuilds (#172). The environment pays the Dedicated plan management fee while it exists: turn it on for a rebuild, off afterwards (docs/operations.md, "Full rebuild").')
param dedicatedProfile bool = false

@description('Run the ingest job on the E4 profile (needs dedicatedProfile). Turn this off and provision before turning dedicatedProfile off: a profile in use can\'t be removed.')
param ingestOnDedicated bool = false

@description('Backfill schedule, UTC cron (e.g. "0 9 2-4 10 *" while a backfill lasts). Empty: run the job manually.')
param backfillCron string = ''

@description('Size in GiB of the NFS share the ingest job\'s Quickwit writer works on (08 §8.4): 128 or more for the merge settings in pages-index.yaml; 0: the replica\'s own disk, too small to merge large indexes.')
param ingestScratchGiB int = 128

@description('Seconds an ingest release waits for its new index\'s merges before failing without publishing (08 §8.4).')
param ingestMergeTimeoutSecs int = 14400

@description('Cosmos DB free tier (one per subscription). False makes the account serverless.')
param cosmosFreeTier bool = true

@description('Comma-separated alert and budget recipients. Budget and alerts are skipped when empty.')
param alertEmails string = ''


@description('Budget start month, YYYY-MM-01. Set once per environment (a budget\'s start date cannot change); the budget is skipped when empty.')
param budgetStartDate string = ''


@description('Comma-separated Entra object ids (people or groups) that may read the search log in the searches container (scripts/searches.sh).')
param searchLogReaders string = ''

@description('Deploy Azure AI Document Intelligence for OCR of Japanese pages (#128).')
param ocr bool = false

@description('Document Intelligence tier: F0 (500 pages a month free) or S0.')
@allowed(['F0', 'S0'])
param ocrSku string = 'F0'

@description('Entra object ids (people or groups) that may call Document Intelligence, comma separated.')
param ocrUsers string = ''

@description('Deploy the Japanese OCR job (caj-usnm-jaocr, #128). Needs the usnewsmap-ja-ocr image in the registry (CI publishes it from main).')
param jaOcrJob bool = false

@description('Parallel replicas of the Japanese OCR job.')
@minValue(1)
@maxValue(8)
param jaOcrReplicas int = 2

@description('Assign the guard-rail policies (defined by the usnm-guardrails stack, infra/guardrails.bicep) to the resource groups.')
param deployPolicies bool = true

@description('Seconds between availability test runs of the site and API from each of 3 locations (tests exist only with dnsZoneName and alert emails). 300, 600 or 900. Each run costs $0.0005: 300 is about $13 a month per URL.')
@allowed(['300', '600', '900'])
param availabilityFrequency string = '900'

@description('Public DNS zone for the site (e.g. usnewsmap.com); empty skips it. Delegate the domain to the NAME_SERVERS output.')
param dnsZoneName string = ''
@description('More TXT values at the zone apex, keyed by zone name (site-verification tokens). They are public, so they live in git.')
param dnsApexTxtValues object = {}

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
    // Prod: 1 GB. 0.15 GB stopped all logging on 2026-10-07 (the American Stories writer's blob
    // writes reached it by 17:46 ET; nothing was logged, alerts included, until the next reset).
    // A normal prod day is about 80 MB, inside the free 5 GB/month; the cap bounds bursts from
    // bulk jobs. Dev keeps 0.15 GB, inside the free allowance at any rate (ADR-0005).
    dailyCapGb: env == 'prod' ? '1' : '0.15'
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

// The tiles account serves public map data. The public-network policy exempts it
// by resource id, so its assignment must be in place before the account is written.
var tilesName = 'stusnmt${suffix}'
var tilesId = resourceId(subscription().subscriptionId, rg.name, 'Microsoft.Storage/storageAccounts', tilesName)

module tiles 'modules/tiles.bicep' = {
  scope: rg
  name: 'tiles'
  dependsOn: [policies]
  params: {
    location: location
    tags: union(tags, { 'usnm-public': 'true' })
    name: tilesName
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
    appInsightsName: monitoring.outputs.appInsightsName
    appPrincipalId: identities.outputs.appPrincipalId
    ingestPrincipalId: identities.outputs.ingestPrincipalId
    searchLogReaders: filter(map(split(searchLogReaders, ','), r => trim(r)), r => !empty(r))
  }
}

module ocrService 'modules/ocr.bicep' = if (ocr) {
  scope: rg
  name: 'ocr'
  params: {
    location: location
    tags: tags
    name: 'di-usnm-${env}-${suffix}'
    sku: ocrSku
    workspaceId: monitoring.outputs.workspaceId
    users: filter(map(split(ocrUsers, ','), u => trim(u)), u => !empty(u))
    identities: [identities.outputs.ingestPrincipalId]
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
    dedicatedProfile: dedicatedProfile
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
    scratchStorageName: ingestScratch ? scratch!.outputs.accountName : ''
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
    cosmosEndpoint: cosmos.outputs.endpoint
    allowedOrigins: siteOrigins
    minReplicas: apiMinReplicas
    searchBackend: searchBackend
    appInsightsConnectionString: monitoring.outputs.appInsightsConnectionString
    ingestCron: ingestJobs && useAcr ? ingestCron : ''
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

var ingestScratch = ingestJobs && useAcr && ingestScratchGiB > 0

module scratch 'modules/ingest-scratch.bicep' = if (ingestScratch) {
  scope: rg
  name: 'ingest-scratch'
  params: {
    location: location
    tags: tags
    name: 'stusnms${suffix}'
    sizeGiB: ingestScratchGiB
    vnetId: network.outputs.vnetId
    vnetName: network.outputs.vnetName
    peSubnetId: network.outputs.peSubnetId
    containerEnvName: containerEnv.outputs.name
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
    appInsightsConnectionString: monitoring.outputs.appInsightsConnectionString
    jobNameSuffix: env
    cron: ingestCron
    full: ingestFull
    americanStories: americanStories
    workers: backfillWorkers
    backfillCron: backfillCron
    scratchStorageName: ingestScratch ? scratch!.outputs.envStorageName : ''
    scratchGiB: ingestScratch ? ingestScratchGiB : 0
    mergeTimeoutSecs: ingestMergeTimeoutSecs
    ingestProfile: ingestOnDedicated ? containerEnv.outputs.dedicatedProfileName : ''
    jaOcrImage: jaOcrJob ? '${registry.outputs.loginServer}/usnewsmap-ja-ocr:${imageTag}' : ''
    jaOcrReplicas: jaOcrReplicas
    rootImage: '${registry.outputs.loginServer}/quickwit/quickwit@${quickwitDigest}'
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

// Ingest and backfill failures and stalls, from the jobs' console logs.
module ingestAlerts 'modules/ingest-alerts.bicep' = if (!empty(emails) && ingestJobs && useAcr) {
  scope: rg
  name: 'ingest-alerts'
  params: {
    location: location
    tags: tags
    nameSuffix: env
    workspaceId: monitoring.outputs.workspaceId
    actionGroupId: monitoring.outputs.actionGroupId
  }
}

// API server errors and latency, from the requests it exports; availability
// tests of the site and the API on the custom domain once its certificates
// are bound.
module apiAlerts 'modules/api-alerts.bicep' = if (!empty(emails) && deployApi) {
  scope: rg
  name: 'api-alerts'
  params: {
    location: location
    tags: tags
    nameSuffix: env
    workspaceId: monitoring.outputs.workspaceId
    appInsightsId: monitoring.outputs.appInsightsId
    actionGroupId: monitoring.outputs.actionGroupId
    siteUrl: !empty(dnsZoneName) && !empty(siteCertificateId) ? 'https://${dnsZoneName}/' : ''
    availabilityFrequency: availabilityFrequency
  }
}

// Dashboards over the workspace: the pipeline workbook with the jobs, the
// API workbook with the API. No alert emails needed.
module workbooks 'modules/workbooks.bicep' = if ((ingestJobs && useAcr) || deployApi) {
  scope: rg
  name: 'workbooks'
  params: {
    location: location
    tags: tags
    workspaceId: monitoring.outputs.workspaceId
    appInsightsId: monitoring.outputs.appInsightsId
    pipeline: ingestJobs && useAcr
    api: deployApi
  }
}

// The guard-rail definitions are shared by every environment in the
// subscription and deployed by their own stack (guardrails.bicep); this
// environment only assigns them.
var guardrailPolicyIds = map(
  items(guardrailPolicyNames),
  p => subscriptionResourceId('Microsoft.Authorization/policyDefinitions', p.value)
)

module policies 'modules/policy-assignments.bicep' = if (deployPolicies) {
  scope: rg
  name: 'policy-assignments'
  params: {
    definitionIds: guardrailPolicyIds
    definitionParameters: {
      // Deny and the exemption arrive together, in the same assignment update.
      '${guardrailPolicyNames.auditPublicAccess}': { effect: { value: 'deny' }, publicAccountIds: { value: [tilesId] } }
    }
  }
}

module spotPolicies 'modules/policy-assignments.bicep' = if (deployPolicies) {
  scope: spotRg
  name: 'policy-assignments-spot'
  params: {
    definitionIds: guardrailPolicyIds
    // No public accounts live here.
    definitionParameters: {
      '${guardrailPolicyNames.auditPublicAccess}': { effect: { value: 'deny' } }
    }
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
    apexTxtValues: dnsApexTxtValues[?dnsZoneName] ?? []
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
output JA_OCR_JOB string = ingestJobs && useAcr ? ingest!.outputs.jaOcrJobName : ''
output JA_AUDIT_JOB string = ingestJobs && useAcr ? ingest!.outputs.jaAuditJobName : ''
output ACR_NAME string = registry.outputs.name
output ACR_LOGIN_SERVER string = registry.outputs.loginServer
// For the GitHub Environment variables CI signs in with (not secrets; scripts/bootstrap.sh writes them).
output CI_CLIENT_ID string = registry.outputs.ciClientId
output AZURE_TENANT_ID string = tenant().tenantId
output AZURE_SUBSCRIPTION_ID string = subscription().subscriptionId
output APPLICATIONINSIGHTS_CONNECTION_STRING string = monitoring.outputs.appInsightsConnectionString
// The workspace's customer id, for the Log Analytics query API (scripts/logs.sh).
output LOG_ANALYTICS_WORKSPACE_ID string = monitoring.outputs.workspaceCustomerId
output OCR_ENDPOINT string = ocr ? ocrService!.outputs.endpoint : ''
