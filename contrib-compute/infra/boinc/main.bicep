// DRAFT. Never deployed; compile and lint only (az bicep build).
// The BOINC work-distribution server for usnewsmap OCR: one Container App, a private
// MySQL flexible server, two storage accounts, Key Vault, Log Analytics and a static
// site. Design and cost: docs/design.md. Deployment: scripts/boinc/bootstrap.sh.
targetScope = 'resourceGroup'

@description('Short lower-case name used in resource names.')
@minLength(3)
@maxLength(12)
param name string = 'boinc'

param location string = resourceGroup().location

@description('Server image, by digest, for example registry/name@sha256:...')
param image string

@secure()
@description('MySQL admin password. The bootstrap script generates it and keeps it in Key Vault.')
param mysqlAdminPassword string

@secure()
@description('Key of the Azure Files account. Needed by the Container Apps mount; see modules/storage.bicep.')
param filesAccountKey string

@description('Public host name of the BOINC master URL. Defaults to the Container App address when empty.')
param masterHost string = ''

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: 'usnm-${name}-id'
  location: location
}

module monitor 'modules/monitor.bicep' = {
  name: 'monitor'
  params: { name: name, location: location }
}

module network 'modules/network.bicep' = {
  name: 'network'
  params: { name: name, location: location }
}

module vault 'modules/keyvault.bicep' = {
  name: 'vault'
  params: {
    name: name
    location: location
    readerPrincipalId: identity.properties.principalId
    logAnalyticsWorkspaceId: monitor.outputs.id
  }
}

module storage 'modules/storage.bicep' = {
  name: 'storage'
  params: { name: name, location: location, appPrincipalId: identity.properties.principalId }
}

module mysql 'modules/mysql.bicep' = {
  name: 'mysql'
  params: {
    name: name
    location: location
    subnetId: network.outputs.mysqlSubnetId
    privateDnsZoneId: network.outputs.mysqlDnsZoneId
    adminPassword: mysqlAdminPassword
  }
}

module frontend 'modules/frontend.bicep' = {
  name: 'frontend'
  params: { name: name }
}

resource workspace 'Microsoft.OperationalInsights/workspaces@2023-09-01' existing = {
  name: 'usnm-${name}-log'
}

module app 'modules/app.bicep' = {
  name: 'app'
  params: {
    name: name
    location: location
    acaSubnetId: network.outputs.acaSubnetId
    logAnalyticsCustomerId: monitor.outputs.customerId
    logAnalyticsSharedKey: workspace.listKeys().primarySharedKey
    identityId: identity.id
    identityClientId: identity.properties.clientId
    image: image
    keyVaultName: vault.outputs.name
    filesAccountName: storage.outputs.filesAccountName
    shareName: storage.outputs.shareName
    filesAccountKey: filesAccountKey
    mysqlHost: mysql.outputs.host
    databaseName: mysql.outputs.databaseName
    stagingBlobEndpoint: storage.outputs.stagingBlobEndpoint
    masterHost: masterHost
  }
}

output appFqdn string = app.outputs.fqdn
output staticSiteHost string = frontend.outputs.defaultHostname
output keyVaultName string = vault.outputs.name
output mysqlHost string = mysql.outputs.host
