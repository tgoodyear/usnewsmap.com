// Hosting for the stock BOINC server that the usnm-boinc bridge (crates/usnm-boinc, ocr/boinc)
// submits to: one Container App for Apache/PHP and the stock daemons, a manual-trigger bridge
// job that runs usnm-boinc submit next to it, a job that creates the project's secrets inside
// the VNet, a private MySQL flexible server, one Azure Files share, Key Vault, a registry for
// the server images, Log Analytics and a static site. No BOINC code is built here.
//
// Deployed as the deployment stack usnm-boinc-<env> into the group usnm-boinc-rg-<env>, which
// infra/main.bicep creates (deployBoincGroup) with the guardrails and two waivers (ADR-0019,
// decision 5). Settings: env/<env>.json. Deployment: scripts/provision-boinc.sh.
//
// Two phases. Until the settings name the server image, only the base resources are deployed
// (registry, network, vault, storage, database, monitoring), so the images can be built into
// the registry (scripts/build-boinc-images.sh); with the images set, the app and the jobs too.
targetScope = 'resourceGroup'

import { checkedEnvName, usnmName, usnmCompactName, usnmGlobalName } from '../../../infra/naming.bicep'

@description('Environment: prod or dev (ADR-0017).')
param env string

param location string = resourceGroup().location

@description('The app\'s shared registry (infra/names/<env>.json), which holds the server and bridge images.')
param registryName string

@description('The shared registry\'s resource group.')
param registryResourceGroup string

@description('Existing runtime identity principal, resolved by provision-boinc.sh to make role-assignment IDs deterministic in What-If. Empty on first deployment.')
param runtimePrincipalId string = ''

@description('Existing secrets identity principal, resolved by provision-boinc.sh. Empty on first deployment.')
param keyWriterPrincipalId string = ''

@description('Server image (images/server), by digest, in the shared registry. Empty: deploy the base resources only.')
param image string = ''

@description('Bridge job image (images/bridge), by digest, in the same registry. Required with image: the server image has no usnm-boinc binary.')
param bridgeImage string = ''

@description('Optional web/daemon image override, independent of lifecycle pins. Empty uses assessment selection. Preserves additive production releases.')
param webImage string = ''

@description('A signed app version baked on the server image (images/appver), by digest, in the same registry. images.appver in env/<env>.json, written by scripts/boinc-app-release.sh; empty: no publish job.')
param publishImage string = ''

@description('Run the server from the bridge image (the server image plus usnm-boinc), so the usnm_assess validator (#456) can run. Off by default.')
param assessment bool = false

@description('Deploy the gold bundle job (docs/boinc.md#build-the-gold-bundle): the bridge image running the gold-bundle step, which builds the gold bundle v1 from its publishers onto the share. Off by default.')
param goldBundle bool = false

@description('The usnm_quality wave runner script\'s blob URL (qualityWave.script in env/<env>.json); empty: no quality wave job.')
param qualityWaveScript string = ''

@description('The runner script\'s sha256 (qualityWave.sha256); the job stops if the downloaded script has any other.')
param qualityWaveScriptSha256 string = ''

@description('Seconds before a bridge job run is stopped. assess-stage fetches at 5 s per request, so a longer limit lets one run stage more pages.')
param bridgeTimeoutSeconds int = 1800

@secure()
@description('MySQL admin password. A new one on every deployment, written to Key Vault by this template (mysql-admin-password); nothing outside the deployment sees it.')
param mysqlAdminPassword string = 'Aa1-${newGuid()}'

@description('Require MySQL TLS only after the #464 acceptance/migration/rollback gates and #529 deploy coordination. Defaults OFF; normal provisioning must not activate it implicitly.')
param requireSecureTransport bool = false

@description('Public host name of the BOINC master URL, for example boinc.usnewsmap.com. It cannot change once volunteers attach.')
@minLength(4)
param masterHost string

@description('The app\'s public DNS zone (infra/env/<env>.json USNM_DNS_ZONE) and its group; empty skips the custom domain.')
param dnsZoneName string = ''
param dnsZoneResourceGroup string = ''

@description('Custom domain stage (modules/app.bicep); provision-boinc.sh steps it from 0 to 2.')
@allowed([0, 1, 2])
param customDomainStage int = 0

@description('Entra object ids (people or groups) given Key Vault Secrets Officer on the vault and Log Analytics Reader on the workspace.')
param operatorPrincipalIds array

@description('The PUBLIC half of the offline code-signing key (code_sign_public). Empty until it is made offline.')
param codeSignPublicKey string = ''

@description('The project share\'s billing (#917): standard, the pay-as-you-go StorageV2 share; copy, the provisioned v2 SSD share is created and the server also mounts it at /project-v2 to fill it; provisioned, /project is the provisioned share everywhere and the old account is kept for rollback; provisioned-final, the old account is removed. docs/boinc.md, "Moving the project share".')
@allowed([ 'standard', 'copy', 'provisioned', 'provisioned-final' ])
param projectShare string = 'standard'
@description('Public IPv4 addresses the BOINC Key Vault admits on its public endpoint (the home network); empty keeps it private-only. modules/keyvault.bicep.')
param keyVaultAllowedIps array = []

@description('Create the workbook "usnm BOINC (<env>)" over the workspace (modules/workbook.bicep). On by default in prod.')
param workbook bool = env == 'prod'

var envName = checkedEnvName(env)
var sfx = take(uniqueString(subscription().id, envName, 'boinc', location), 6)
var withApp = !empty(image)

var names = {
  app: usnmName('boinc', 'ca', envName)
  bridgeJob: usnmName('boinc-bridge', 'caj', envName)
  environment: usnmName('boinc', 'cae', envName)
  files: usnmCompactName('boinc', 'st', envName, sfx)
  filesEndpoint: usnmName('boinc-files', 'pep', envName)
  filesV2: usnmCompactName('boincv2', 'st', envName, sfx)
  filesV2Endpoint: usnmName('boinc-files-v2', 'pep', envName)
  shareCopyJob: usnmName('boinc-sharecopy', 'caj', envName)
  shareCopyIdentity: usnmName('boinc-sharecopy', 'id', envName)
  identity: usnmName('boinc', 'id', envName)
  mysql: usnmGlobalName('boinc', 'mysql', envName, sfx)
  mysqlDnsZone: 'usnm-boinc-${envName}.private.mysql.database.azure.com'
  initJob: usnmName('boinc-init', 'caj', envName)
  goldBundleJob: usnmName('boinc-gold', 'caj', envName)
  qualityWaveJob: usnmName('boinc-quality', 'caj', envName)
  publishJob: usnmName('boinc-publish', 'caj', envName)
  secretsIdentity: usnmName('boinc-secrets', 'id', envName)
  secretsJob: usnmName('boinc-secrets', 'caj', envName)
  site: usnmName('boinc', 'swa', envName)
  vault: usnmCompactName('boinc', 'kv', envName, sfx)
  vaultEndpoint: usnmName('boinc-vault', 'pep', envName)
  vnet: usnmName('boinc', 'vnet', envName)
  workspace: usnmName('boinc', 'log', envName)
  workbookTitle: 'usnm BOINC (${envName})'
}

var registryServer = '${registryName}${environment().suffixes.acrLoginServer}'

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: names.identity
  location: location
}

// The secrets job's own identity: Secrets Officer on the vault, which the app's does not need.
resource secretsIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: names.secretsIdentity
  location: location
}

// The share copy job's own identity (projectShare copy): writes the provisioned share, reads
// the old one through its mount, and pulls the bridge image. Nothing else.
resource shareCopyIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = if (projectShare == 'copy') {
  name: names.shareCopyIdentity
  location: location
}

var runtimePrincipal = empty(runtimePrincipalId) ? identity.properties.principalId : runtimePrincipalId
var secretsPrincipal = empty(keyWriterPrincipalId) ? secretsIdentity.properties.principalId : keyWriterPrincipalId

module registry 'modules/registry.bicep' = {
  name: 'boinc-registry-pull'
  scope: resourceGroup(registryResourceGroup)
  params: {
    registryName: registryName
    pullPrincipalIds: concat([ runtimePrincipal, secretsPrincipal ], projectShare == 'copy' ? [ shareCopyIdentity!.properties.principalId ] : [])
  }
}

module monitor 'modules/monitor.bicep' = {
  name: 'monitor'
  params: { workspaceName: names.workspace, location: location, operatorPrincipalIds: operatorPrincipalIds }
}

module workbookModule 'modules/workbook.bicep' = if (workbook) {
  name: 'workbook'
  params: { location: location, workspaceId: monitor.outputs.id, title: names.workbookTitle }
}

module network 'modules/network.bicep' = {
  name: 'network'
  params: { vnetName: names.vnet, mysqlDnsZoneName: names.mysqlDnsZone, location: location }
}

module vaultExemption 'modules/keyvault-exemption.bicep' = if (!empty(keyVaultAllowedIps)) {
  name: 'vault-exemption'
  params: {
    vaultName: names.vault
    policyAssignmentId: '${resourceGroup().id}/providers/Microsoft.Authorization/policyAssignments/usnm-audit-data-public-network'
  }
}

module vault 'modules/keyvault.bicep' = {
  name: 'vault'
  dependsOn: [ vaultExemption ]
  params: {
    vaultName: names.vault
    endpointName: names.vaultEndpoint
    location: location
    readerPrincipalId: runtimePrincipal
    writerPrincipalId: secretsPrincipal
    peSubnetId: network.outputs.peSubnetId
    dnsZoneId: network.outputs.vaultDnsZoneId
    operatorPrincipalIds: operatorPrincipalIds
    logAnalyticsWorkspaceId: monitor.outputs.id
    mysqlAdminPassword: mysqlAdminPassword
    codeSignPublicKey: codeSignPublicKey
    allowedIps: keyVaultAllowedIps
  }
}

module storage 'modules/storage.bicep' = {
  name: 'storage'
  params: {
    filesAccountName: names.files
    endpointName: names.filesEndpoint
    location: location
    // Raised by hand to 250 GB for the quality wave's bundles; kept here so a provision
    // doesn't shrink it under live data (#529). The provisioned share is the same size.
    shareQuotaGb: 250
    legacyShare: projectShare != 'provisioned-final'
    provisionedShare: projectShare != 'standard'
    provisionedAccountName: names.filesV2
    provisionedEndpointName: names.filesV2Endpoint
    copyWriterPrincipalId: projectShare == 'copy' ? shareCopyIdentity!.properties.principalId : ''
    peSubnetId: network.outputs.peSubnetId
    dnsZoneId: network.outputs.filesDnsZoneId
  }
}

module mysql 'modules/mysql.bicep' = {
  name: 'mysql'
  params: {
    serverName: names.mysql
    location: location
    subnetId: network.outputs.mysqlSubnetId
    privateDnsZoneId: network.outputs.mysqlDnsZoneId
    adminPassword: mysqlAdminPassword
    requireSecureTransport: requireSecureTransport
  }
}

module frontend 'modules/frontend.bicep' = {
  name: 'frontend'
  params: { siteName: names.site }
}

// The Container Apps mount takes the account key and the environment's log configuration the
// workspace key (ADR-0019, decision 5, waived for this group). Both are read here, inside the
// deployment, from resources this template creates; no key is passed in, printed or committed.
// Which account /project is, by stage; during the copy the provisioned one is also mounted.
// The app module reads the account's key itself, after the storage module has made it (a key
// read here, through an `existing` reference, came first, and the first deployment of the copy
// stage failed with ResourceNotFound).
var projectOnV2 = projectShare == 'provisioned' || projectShare == 'provisioned-final'

resource workspace 'Microsoft.OperationalInsights/workspaces@2023-09-01' existing = {
  name: names.workspace
}

module app 'modules/app.bicep' = if (withApp) {
  name: 'app'
  dependsOn: [ registry ]
  params: {
    environmentName: names.environment
    appName: names.app
    location: location
    acaSubnetId: network.outputs.acaSubnetId
    logAnalyticsCustomerId: monitor.outputs.customerId
    logAnalyticsSharedKey: workspace.listKeys().primarySharedKey
    identityId: identity.id
    identityClientId: identity.properties.clientId
    image: !empty(webImage) ? webImage : (assessment ? bridgeImage : image)
    // The bridge image's entry point is bridge-job; the server keeps its own.
    command: assessment || !empty(webImage) ? [ '/usr/local/lib/usnm-boinc/entrypoint.sh', '--serve' ] : []
    registryServer: registryServer
    keyVaultName: vault.outputs.name
    filesAccountName: projectOnV2 ? names.filesV2 : names.files
    // A new storage name on each switch (and back, on rollback) forces a new revision.
    projectStorageName: projectOnV2 ? 'project-v2' : 'project'
    shareName: storage.outputs.shareName
    mysqlHost: mysql.outputs.host
    databaseName: mysql.outputs.databaseName
    masterHost: masterHost
    customDomainStage: empty(dnsZoneName) ? 0 : customDomainStage
  }
}

module customDomainDns 'modules/custom-domain-dns.bicep' = if (withApp && !empty(dnsZoneName)) {
  name: 'boinc-custom-domain-dns'
  scope: resourceGroup(dnsZoneResourceGroup)
  params: {
    zoneName: dnsZoneName
    recordName: substring(masterHost, 0, length(masterHost) - length(dnsZoneName) - 1)
    appFqdn: app!.outputs.fqdn
    verificationId: app!.outputs.verificationId
  }
}

module bridgeJob 'modules/bridge-job.bicep' = if (withApp) {
  name: 'bridge-job'
  dependsOn: [ registry ]
  params: {
    jobName: names.bridgeJob
    location: location
    environmentId: app!.outputs.environmentId
    identityId: identity.id
    identityClientId: identity.properties.clientId
    image: bridgeImage
    registryServer: registryServer
    keyVaultName: vault.outputs.name
    mysqlHost: mysql.outputs.host
    databaseName: mysql.outputs.databaseName
    masterHost: masterHost
    projectStorageName: app!.outputs.projectStorageName
    workloadProfileName: app!.outputs.workloadProfileName
    timeoutSeconds: bridgeTimeoutSeconds
    extraEnv: env == 'prod' ? [
      { name: 'BRIDGE_STEP', value: 'iris-bundle-collect' }
      { name: 'IRIS_CAMPAIGN', value: 'overproof-100-v12' }
    ] : []
  }
}

// Project init (the schema and the project tree on the share) as its own lifecycle job: the same
// mounts and settings as the bridge, which a per-run command override would drop. The
// entrypoint skips a completed init and refuses to overwrite a partial tree or database.
module initJob 'modules/bridge-job.bicep' = if (withApp) {
  name: 'init-job'
  dependsOn: [ registry ]
  params: {
    jobName: names.initJob
    location: location
    environmentId: app!.outputs.environmentId
    identityId: identity.id
    identityClientId: identity.properties.clientId
    image: image
    registryServer: registryServer
    keyVaultName: vault.outputs.name
    mysqlHost: mysql.outputs.host
    databaseName: mysql.outputs.databaseName
    masterHost: masterHost
    projectStorageName: app!.outputs.projectStorageName
    workloadProfileName: app!.outputs.workloadProfileName
    command: [ '/usr/local/lib/usnm-boinc/entrypoint.sh', '--ensure-project' ]
  }
}

// App version publish (docs/app-release.md): copies the version directory baked into publishImage
// onto the share and runs bin/update_versions. Its own job, like init, because a per-run image
// override would drop the share and the settings. Deployed whenever images.appver is set, so it stays
// across deployments and always holds the last published version.
module publishJob 'modules/bridge-job.bicep' = if (withApp && !empty(publishImage)) {
  name: 'publish-job'
  dependsOn: [ registry ]
  params: {
    jobName: names.publishJob
    location: location
    environmentId: app!.outputs.environmentId
    identityId: identity.id
    identityClientId: identity.properties.clientId
    image: publishImage
    registryServer: registryServer
    keyVaultName: vault.outputs.name
    mysqlHost: mysql.outputs.host
    databaseName: mysql.outputs.databaseName
    masterHost: masterHost
    projectStorageName: app!.outputs.projectStorageName
    workloadProfileName: app!.outputs.workloadProfileName
    command: [ '/usr/local/lib/usnm-boinc/publish-appver.sh' ]
    // Publish into the live project, keeping its credentials and config links
    // (contrib-compute/docs/amd-885-release.md); set by hand for the AMD release (#529).
    extraEnv: [ { name: 'APPVER_REUSE_PROJECT', value: '1' } ]
  }
}

// The gold bundle v1, built inside the VNet from the datasets' publishers onto the share
// (gold-bundle.sh): the bridge job's image, mounts and settings, its step fixed in the job, so a
// plain `az containerapp job start` runs it. Six hours: gold.py paces its downloads, and the
// datasets are unpacked on the share.
module goldBundleJob 'modules/bridge-job.bicep' = if (withApp && goldBundle) {
  name: 'gold-bundle-job'
  dependsOn: [ registry ]
  params: {
    jobName: names.goldBundleJob
    location: location
    environmentId: app!.outputs.environmentId
    identityId: identity.id
    identityClientId: identity.properties.clientId
    image: bridgeImage
    registryServer: registryServer
    keyVaultName: vault.outputs.name
    mysqlHost: mysql.outputs.host
    databaseName: mysql.outputs.databaseName
    masterHost: masterHost
    projectStorageName: app!.outputs.projectStorageName
    workloadProfileName: app!.outputs.workloadProfileName
    timeoutSeconds: 21600
    cpu: '1'
    memory: '2Gi'
    extraEnv: [
      { name: 'BRIDGE_STEP', value: 'gold-bundle' }
      { name: 'GOLD_CAMPAIGN', value: 'v1-tess550' }
    ]
  }
}

// The usnm_quality wave (scripts/boinc/quality_corpus_runner.py, uploaded by boinc-quality-campaign.py launch-corpus): eight shards
// that stage page bundles onto the share and collect results. The script is fetched with the
// BOINC identity and run only if its sha256 is the committed one. 4 vCPUs: the runner converts
// any page the on-prem JPEG cache (docs/boinc.md) hasn't; more cache, not more vCPUs, is the
// way to speed it up. A day per run; it is started by hand (az containerapp job start).
// It stages only pages the scan archive holds and then ends: the LoC fallback pass, one
// 5-second pacer across every shard, is off unless QUALITY_LOC_FALLBACK=1 (#914).
module qualityWaveJob 'modules/bridge-job.bicep' = if (withApp && !empty(qualityWaveScript)) {
  name: 'quality-wave-job'
  dependsOn: [ registry ]
  params: {
    jobName: names.qualityWaveJob
    location: location
    environmentId: app!.outputs.environmentId
    identityId: identity.id
    identityClientId: identity.properties.clientId
    image: bridgeImage
    registryServer: registryServer
    keyVaultName: vault.outputs.name
    mysqlHost: mysql.outputs.host
    databaseName: mysql.outputs.databaseName
    masterHost: masterHost
    projectStorageName: app!.outputs.projectStorageName
    workloadProfileName: app!.outputs.workloadProfileName
    timeoutSeconds: 86400
    cpu: '4'
    memory: '8Gi'
    extraEnv: [
      { name: 'QUALITY_WAVE_SCRIPT', value: qualityWaveScript }
      { name: 'QUALITY_WAVE_SHA256', value: qualityWaveScriptSha256 }
    ]
    command: [
      '/bin/bash'
      '-c'
      'set -euo pipefail; tok="$(curl -fsS -H "X-IDENTITY-HEADER: $IDENTITY_HEADER" "$IDENTITY_ENDPOINT?api-version=2019-08-01&resource=https://storage.azure.com/&client_id=$AZURE_CLIENT_ID" | jq -er .access_token)"; curl -fsSL -H "Authorization: Bearer $tok" -H "x-ms-version: 2021-08-06" "$QUALITY_WAVE_SCRIPT" -o /tmp/run_wave9.py; echo "$QUALITY_WAVE_SHA256  /tmp/run_wave9.py" | sha256sum -c -; export NUM_SHARDS=8 BUNDLE_SIZE=200 MAX_IN_FLIGHT_BUNDLES=45 MIN_FREE_GB=35.0; pids=""; for s in 0 1 2 3 4 5 6 7; do SHARD_ENV=$s python3 /tmp/run_wave9.py & pids="$pids $!"; sleep 3; done; rc=0; for p in $pids; do wait "$p" || rc=1; done; exit "$rc"'
    ]
  }
}

// The share copy job (projectShare copy; docs/boinc.md, "Moving the project share"): AzCopy,
// pinned by version and sha256, syncs the old share to the provisioned one, both over REST
// through their private endpoints, as the job's identity. Over REST, BOINC's symlinks are the
// mfsymlinks files that store them, so they copy byte for byte. Run it while live, then once more with the daemons
// stopped; sync sends only what changed and deletes what's gone.
var azcopyUrl = 'https://github.com/Azure/azure-storage-azcopy/releases/download/v10.32.8/azcopy_linux_amd64_10.32.8.tar.gz'
var azcopySha256 = 'a95277dbc265912cefdddbaf251aa99ec648cb18ba657e8788066357a9022dc3'
module shareCopyJob 'modules/bridge-job.bicep' = if (withApp && projectShare == 'copy') {
  name: 'share-copy-job'
  dependsOn: [ registry ]
  params: {
    jobName: names.shareCopyJob
    location: location
    environmentId: app!.outputs.environmentId
    identityId: shareCopyIdentity.id
    identityClientId: shareCopyIdentity!.properties.clientId
    image: bridgeImage
    registryServer: registryServer
    keyVaultName: vault.outputs.name
    mysqlHost: mysql.outputs.host
    databaseName: mysql.outputs.databaseName
    masterHost: masterHost
    projectStorageName: app!.outputs.projectStorageName
    workloadProfileName: app!.outputs.workloadProfileName
    timeoutSeconds: 21600
    // Walking 345,000 files over the SMB mount was the whole cost of a pass (156 minutes to
    // send 28 files, 107 with 4 cores and 32 scanners); the source is now listed over REST.
    cpu: '4'
    memory: '8Gi'
    mountProject: false
    extraEnv: [
      { name: 'AZCOPY_URL', value: azcopyUrl }
      { name: 'AZCOPY_SHA256', value: azcopySha256 }
      { name: 'SHARE_COPY_SRC', value: 'https://${names.files}.file.${environment().suffixes.storage}/project' }
      { name: 'SHARE_COPY_DEST', value: 'https://${names.filesV2}.file.${environment().suffixes.storage}/project' }
      { name: 'AZCOPY_CONCURRENT_SCAN', value: '32' }
    ]
    command: [
      '/bin/bash'
      '-c'
      'set -euo pipefail; cd /tmp; curl -fsSL -o azcopy.tgz "$AZCOPY_URL"; echo "$AZCOPY_SHA256  azcopy.tgz" | sha256sum -c -; tar -xzf azcopy.tgz --strip-components=1; export AZCOPY_AUTO_LOGIN_TYPE=MSI AZCOPY_MSI_CLIENT_ID="$AZURE_CLIENT_ID" AZCOPY_LOG_LOCATION=/tmp/azcopy-logs AZCOPY_JOB_PLAN_LOCATION=/tmp/azcopy-plans; ./azcopy --version; rc=0; ./azcopy sync "$SHARE_COPY_SRC" "$SHARE_COPY_DEST" --recursive --delete-destination=true --output-level=essential || rc=$?; jid=$(./azcopy jobs list | sed -n "s/^JobId: //p" | head -1); [ -z "$jid" ] || ./azcopy jobs show "$jid" --with-status=Failed | head -60; exit "$rc"'
    ]
  }
}

module secretsJob 'modules/secrets-job.bicep' = if (withApp) {
  name: 'secrets-job'
  dependsOn: [ registry ]
  params: {
    jobName: names.secretsJob
    location: location
    environmentId: app!.outputs.environmentId
    identityId: secretsIdentity.id
    identityClientId: secretsIdentity.properties.clientId
    image: image
    registryServer: registryServer
    keyVaultName: vault.outputs.name
    workloadProfileName: app!.outputs.workloadProfileName
  }
}

output appFqdn string = withApp ? app!.outputs.fqdn : ''
output bridgeJobName string = withApp ? bridgeJob!.outputs.name : ''
output goldBundleJobName string = withApp && goldBundle ? goldBundleJob!.outputs.name : ''
output qualityWaveJobName string = withApp && !empty(qualityWaveScript) ? qualityWaveJob!.outputs.name : ''
output initJobName string = withApp ? initJob!.outputs.name : ''
output keyVaultName string = vault.outputs.name
output mysqlHost string = mysql.outputs.host
output logAnalyticsQueryHost string = monitor.outputs.logAnalyticsQueryHost
output logAnalyticsQueryHosts array = monitor.outputs.logAnalyticsQueryHosts
output publishJobName string = withApp && !empty(publishImage) ? publishJob!.outputs.name : ''
output registryName string = registryName
output registryServer string = registryServer
output secretsJobName string = withApp ? secretsJob!.outputs.name : ''
output staticSiteHost string = frontend.outputs.defaultHostname
