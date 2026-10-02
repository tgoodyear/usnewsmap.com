// Scratch disk for the Quickwit writer in caj-usnm-ingest (08 §8.4): an NFS
// Azure Files share, mounted by the job, for the writer's write-ahead log,
// the splits it builds and the merges it runs. A Container Apps replica's
// own disk is 8 GiB by the documentation (about 19.5 GiB in practice), and
// the September 2026 releases filled it: indexers died with "No space left
// on device" and merges failed. The largest merge needs its inputs and its
// output on disk at once, about twice `split_num_docs_target` pages
// (infra/quickwit/pages-index.yaml): about 60 GB at 23 KB a page.
//
// NFS uses no keys, but no identity either: access is by network, through
// the private endpoint in the VNet, so anything in the Container Apps
// subnet can reach the share. That is an exception to ADR-0009, recorded
// there with its limits. Shared key access stays disabled. Container Apps
// can't mount NFS shares that require encryption in transit, so that is
// turned off for NFS; nothing leaves the VNet, and HTTPS stays required for
// the REST API.

param location string
param tags object
param name string
@description('Share size in GiB. 128 is the floor for the merge settings in pages-index.yaml: a merge of about 60 GB, plus the write-ahead log, the split cache and the split being built, about 70 GB, with room for pages that index larger. A smaller share needs a smaller split_num_docs_target.')
@minValue(128)
param sizeGiB int
param vnetId string
param vnetName string
param peSubnetId string
param containerEnvName string

var shareName = 'qw-scratch'
var zoneName = 'privatelink.file.${environment().suffixes.storage}'

resource account 'Microsoft.Storage/storageAccounts@2025-01-01' = {
  name: name
  location: location
  tags: tags
  kind: 'FileStorage'
  // Provisioned v2 SSD: billed by the GiB provisioned (about $0.10/GiB a
  // month), with baseline IOPS and throughput included.
  sku: { name: 'PremiumV2_LRS' }
  properties: {
    allowSharedKeyAccess: false
    allowBlobPublicAccess: false
    allowCrossTenantReplication: false
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    publicNetworkAccess: 'Disabled'
    networkAcls: {
      defaultAction: 'Deny'
      bypass: 'None'
    }
  }
}

resource files 'Microsoft.Storage/storageAccounts/fileServices@2025-01-01' = {
  parent: account
  name: 'default'
  properties: {
    protocolSettings: { nfs: { encryptionInTransit: { required: false } } }
  }
}

resource share 'Microsoft.Storage/storageAccounts/fileServices/shares@2025-01-01' = {
  parent: files
  name: shareName
  properties: {
    enabledProtocols: 'NFS'
    // The job's init container runs as root to hand a directory to the
    // pipeline's user (uid 10001).
    rootSquash: 'NoRootSquash'
    shareQuota: sizeGiB
  }
}

resource zone 'Microsoft.Network/privateDnsZones@2024-06-01' = {
  name: zoneName
  location: 'global'
  tags: tags
}

resource link 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2024-06-01' = {
  parent: zone
  name: '${vnetName}-link'
  location: 'global'
  tags: tags
  properties: {
    registrationEnabled: false
    virtualNetwork: { id: vnetId }
  }
}

resource pe 'Microsoft.Network/privateEndpoints@2024-05-01' = {
  name: 'pe-usnm-file'
  location: location
  tags: tags
  properties: {
    subnet: { id: peSubnetId }
    privateLinkServiceConnections: [
      {
        name: 'pe-usnm-file'
        properties: {
          privateLinkServiceId: account.id
          groupIds: ['file']
        }
      }
    ]
  }
}

resource zoneGroup 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-05-01' = {
  parent: pe
  name: 'default'
  properties: {
    privateDnsZoneConfigs: [
      {
        name: 'file'
        properties: { privateDnsZoneId: zone.id }
      }
    ]
  }
}

resource containerEnv 'Microsoft.App/managedEnvironments@2025-01-01' existing = {
  name: containerEnvName
}

// The share as the environment's jobs mount it.
resource envStorage 'Microsoft.App/managedEnvironments/storages@2025-01-01' = {
  parent: containerEnv
  name: 'ingest-scratch'
  dependsOn: [share, zoneGroup, link]
  properties: {
    nfsAzureFile: {
      server: '${account.name}.file.${environment().suffixes.storage}'
      shareName: '/${account.name}/${shareName}'
      accessMode: 'ReadWrite'
    }
  }
}

output accountName string = account.name
output envStorageName string = envStorage.name
