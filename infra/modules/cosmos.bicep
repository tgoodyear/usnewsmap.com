// Document state and the ingest work queue (05 §5.9.1, ADR-0007): Entra ID
// only, private endpoint only in steady state.
//
// With allowedIps, public network access is on behind an IP firewall that
// allows those addresses alone: the operator's home IP, from USNM_OCR_NODE_IPS,
// never in the repository. Two settings open it: onPremServing (the on-prem
// serving container, #955) and cosmosOnPremAccess (the on-prem BOINC host's
// claim and collect, #967). Requests from anywhere else get 403. The private
// endpoint stays, and its traffic is authorized by the endpoint, not by
// ipRules, so the Container Apps keep reaching the account through it
// (https://learn.microsoft.com/en-us/azure/cosmos-db/how-to-configure-private-endpoints#private-link-combined-with-firewall-rules).
// No 0.0.0.0 rule (every Azure datacenter) and no trusted-service bypass
// (https://learn.microsoft.com/en-us/azure/cosmos-db/how-to-configure-firewall).
// The public-network policy exempts the account by resource id while it is
// open (main.bicep, public-accounts.bicep).

param location string
param tags object
param name string
@description('Free tier (1000 RU/s, 25 GB, continuous backup): one account per subscription. When false, the account is serverless with periodic backup.')
param freeTier bool = true
@description('Public IPv4 addresses or CIDR ranges allowed through the IP firewall (the on-prem hosts\' public IP). Empty: public network access disabled, the private endpoint only. Never empty with access on: an account with public access and no ipRules accepts every network.')
param allowedIps array = []

var open = !empty(allowedIps)

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
    publicNetworkAccess: open ? 'Enabled' : 'Disabled'
    ipRules: [
      for ip in allowedIps: {
        ipAddressOrRange: endsWith(ip, '/32') ? replace(ip, '/32', '') : ip
      }
    ]
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
