// Document state and the ingest work queue (05 §5.9.1, ADR-0007): Entra ID
// only, private endpoint only in steady state.
//
// With allowedIps (the on-prem serving container, onPremServing, #955),
// public network access is on, and the firewall denies everything but
// those addresses: the operator's home IP, from USNM_OCR_NODE_IPS, never in
// the repository. The private endpoint stays, and the Container Apps keep
// using it. No trusted-service bypass. The public-network policy exempts
// the account by resource id only while it is open (main.bicep,
// public-accounts.bicep).

param location string
param tags object
param name string
@description('Free tier (1000 RU/s, 25 GB, continuous backup): one account per subscription. When false, the account is serverless with periodic backup.')
param freeTier bool = true
@description('Public IPv4 addresses or CIDR ranges allowed through the firewall. Empty (the default) keeps public network access off.')
param allowedIps array = []

var containers = [
  { name: 'titles', key: '/lccn' }
  { name: 'batches', key: '/batch' }
  { name: 'issues', key: '/lccn' }
  { name: 'index_runs', key: '/index_version' }
  { name: 'ops', key: '/kind' }
]

resource account 'Microsoft.DocumentDB/databaseAccounts@2024-11-15' = {
  name: name
  location: location
  tags: tags
  kind: 'GlobalDocumentDB'
  properties: {
    databaseAccountOfferType: 'Standard'
    enableFreeTier: freeTier
    // Retain production's existing setting; omission resets it on redeployment.
    enableAutomaticFailover: freeTier
    capabilities: freeTier ? [] : [{ name: 'EnableServerless' }]
    locations: [{ locationName: location, failoverPriority: 0, isZoneRedundant: false }]
    consistencyPolicy: { defaultConsistencyLevel: 'Session' }
    disableLocalAuth: true
    disableKeyBasedMetadataWriteAccess: true
    publicNetworkAccess: empty(allowedIps) ? 'Disabled' : 'Enabled'
    ipRules: [for ip in allowedIps: { ipAddressOrRange: ip }]
    networkAclBypass: 'None'
    minimalTlsVersion: 'Tls12'
    // Continuous backup (7-day, free) needs provisioned throughput; serverless
    // accounts take periodic backup (every 4 h, 8 h retention, the free default).
    backupPolicy: freeTier
      ? {
          type: 'Continuous'
          continuousModeProperties: { tier: 'Continuous7Days' }
        }
      : {
          type: 'Periodic'
          periodicModeProperties: {
            backupIntervalInMinutes: 240
            backupRetentionIntervalInHours: 8
            backupStorageRedundancy: 'Local'
          }
        }
  }
}

resource db 'Microsoft.DocumentDB/databaseAccounts/sqlDatabases@2024-11-15' = {
  parent: account
  name: 'usnm'
  properties: {
    resource: { id: 'usnm' }
    // Shared throughput equal to the free-tier allowance.
    options: freeTier ? { throughput: 1000 } : {}
  }
}

resource dbContainers 'Microsoft.DocumentDB/databaseAccounts/sqlDatabases/containers@2024-11-15' = [
  for c in containers: {
    parent: db
    name: c.name
    properties: {
      resource: {
        id: c.name
        partitionKey: { paths: [c.key], kind: 'Hash' }
      }
    }
  }
]

output id string = account.id
output name string = account.name
output endpoint string = account.properties.documentEndpoint
output databaseName string = db.name
