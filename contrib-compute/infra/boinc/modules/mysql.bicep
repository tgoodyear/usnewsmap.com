// MySQL flexible server, private access only, Burstable tier.
//
// Authentication gap: BOINC's daemons and PHP code connect with a static user and
// password from config.xml. Entra tokens last 5 to 60 minutes and BOINC cannot refresh
// them, so Entra auth is not usable. The admin password is a new random value on every deployment
// (main.bicep's parameter default), written to Key Vault by the same deployment.
// See docs/design.md, security.
//
// Unverified: BOINC's schema uses MyISAM for a few forum/profile tables, and the
// service supports InnoDB only. Check at the first dev deployment.
param serverName string
param location string
param subnetId string
param privateDnsZoneId string
param adminLogin string = 'usnmboincadmin'
@secure()
param adminPassword string
@description('Burstable B1ms is the plan; B2s if CPU credits run out (docs/design.md, section 6).')
param skuName string = 'Standard_B1ms'
param storageGb int = 20
@description('The BOINC database. make_project creates it (CREATE DATABASE, not IF NOT EXISTS), so this template does not.')
param databaseName string = 'usnmocr'

@description('Enable only after #464 Azure-chain, copied-project migration/rollback and all-consumer acceptance, coordinated under #529. OFF preserves the deployed policy.')
param requireSecureTransport bool = false

resource server 'Microsoft.DBforMySQL/flexibleServers@2024-12-30' = {
  name: serverName
  location: location
  sku: { name: skuName, tier: 'Burstable' }
  properties: {
    version: '8.4'
    administratorLogin: adminLogin
    administratorLoginPassword: adminPassword
    // autoIoScaling pinned: the API defaults it to Enabled (billed per IO) when storage is sent.
    storage: { storageSizeGB: storageGb, autoGrow: 'Enabled', autoIoScaling: 'Disabled', iops: 360 }
    backup: { backupRetentionDays: 7, geoRedundantBackup: 'Disabled' }
    highAvailability: { mode: 'Disabled' }
    network: { delegatedSubnetResourceId: subnetId, privateDnsZoneResourceId: privateDnsZoneId }
  }
}

// Pinned clients now request verified TLS. Existing persisted project copies and the
// deployed Azure certificate chain still need acceptance before enforcement (#464).
resource secureTransport 'Microsoft.DBforMySQL/flexibleServers/configurations@2024-12-30' = {
  parent: server
  name: 'require_secure_transport'
  properties: { value: requireSecureTransport ? 'ON' : 'OFF', source: 'user-override' }
}

// The Entra administrator is NOT created here: it needs a server identity that already holds
// Directory Readers (or Graph permissions). It is not a deployment prerequisite: BOINC uses
// the runtime database account, not Entra authentication (docs/design.md, 5.1).

output host string = server.properties.fullyQualifiedDomainName
output databaseName string = databaseName
