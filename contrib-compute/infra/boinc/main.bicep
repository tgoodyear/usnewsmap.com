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

@description('Server image (images/server), by digest, in this group\'s registry. Empty: deploy the base resources only.')
param image string = ''

@description('Bridge job image (images/bridge), by digest, in the same registry. Required with image: the server image has no usnm-boinc binary.')
param bridgeImage string = ''

@secure()
@description('MySQL admin password. A new one on every deployment, written to Key Vault by this template (mysql-admin-password); nothing outside the deployment sees it.')
param mysqlAdminPassword string = 'Aa1-${newGuid()}'

@description('Public host name of the BOINC master URL, for example boinc.usnewsmap.com. It cannot change once volunteers attach.')
@minLength(4)
param masterHost string

@description('Entra object ids (people or groups) given Key Vault Secrets Officer.')
param operatorPrincipalIds array

@description('The PUBLIC half of the offline code-signing key (code_sign_public). Empty until it is made offline.')
param codeSignPublicKey string = ''

var envName = checkedEnvName(env)
var sfx = take(uniqueString(subscription().id, envName, 'boinc', location), 6)
var withApp = !empty(image)

var names = {
  app: usnmName('boinc', 'ca', envName)
  bridgeJob: usnmName('boinc-bridge', 'caj', envName)
  environment: usnmName('boinc', 'cae', envName)
  files: usnmCompactName('boinc', 'st', envName, sfx)
  filesEndpoint: usnmName('boinc-files', 'pep', envName)
  identity: usnmName('boinc', 'id', envName)
  mysql: usnmGlobalName('boinc', 'mysql', envName, sfx)
  mysqlDnsZone: 'usnm-boinc-${envName}.private.mysql.database.azure.com'
  registry: usnmCompactName('boinc', 'cr', envName, sfx)
  secretsIdentity: usnmName('boinc-secrets', 'id', envName)
  secretsJob: usnmName('boinc-secrets', 'caj', envName)
  site: usnmName('boinc', 'swa', envName)
  vault: usnmCompactName('boinc', 'kv', envName, sfx)
  vaultEndpoint: usnmName('boinc-vault', 'pep', envName)
  vnet: usnmName('boinc', 'vnet', envName)
  workspace: usnmName('boinc', 'log', envName)
}

var registryServer = '${names.registry}${environment().suffixes.acrLoginServer}'

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: names.identity
  location: location
}

// The secrets job's own identity: Secrets Officer on the vault, which the app's does not need.
resource secretsIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: names.secretsIdentity
  location: location
}

module registry 'modules/registry.bicep' = {
  name: 'registry'
  params: {
    registryName: names.registry
    location: location
    pullPrincipalIds: [ identity.properties.principalId, secretsIdentity.properties.principalId ]
  }
}

module monitor 'modules/monitor.bicep' = {
  name: 'monitor'
  params: { workspaceName: names.workspace, location: location }
}

module network 'modules/network.bicep' = {
  name: 'network'
  params: { vnetName: names.vnet, mysqlDnsZoneName: names.mysqlDnsZone, location: location }
}

module vault 'modules/keyvault.bicep' = {
  name: 'vault'
  params: {
    vaultName: names.vault
    endpointName: names.vaultEndpoint
    location: location
    readerPrincipalId: identity.properties.principalId
    writerPrincipalId: secretsIdentity.properties.principalId
    peSubnetId: network.outputs.peSubnetId
    dnsZoneId: network.outputs.vaultDnsZoneId
    operatorPrincipalIds: operatorPrincipalIds
    logAnalyticsWorkspaceId: monitor.outputs.id
    mysqlAdminPassword: mysqlAdminPassword
    codeSignPublicKey: codeSignPublicKey
  }
}

module storage 'modules/storage.bicep' = {
  name: 'storage'
  params: {
    filesAccountName: names.files
    endpointName: names.filesEndpoint
    location: location
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
  }
}

module frontend 'modules/frontend.bicep' = {
  name: 'frontend'
  params: { siteName: names.site }
}

// The Container Apps mount takes the account key and the environment's log configuration the
// workspace key (ADR-0019, decision 5, waived for this group). Both are read here, inside the
// deployment, from resources this template creates; no key is passed in, printed or committed.
resource filesAccount 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: names.files
}

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
    image: image
    registryServer: registryServer
    keyVaultName: vault.outputs.name
    filesAccountName: storage.outputs.filesAccountName
    shareName: storage.outputs.shareName
    filesAccountKey: filesAccount.listKeys().keys[0].value
    mysqlHost: mysql.outputs.host
    databaseName: mysql.outputs.databaseName
    masterHost: masterHost
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
output keyVaultName string = vault.outputs.name
output mysqlHost string = mysql.outputs.host
output registryName string = names.registry
output registryServer string = registryServer
output secretsJobName string = withApp ? secretsJob!.outputs.name : ''
output staticSiteHost string = frontend.outputs.defaultHostname
