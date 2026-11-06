// Workload-profiles environment using the Consumption profile, VNet-integrated
// when subnetId is set. Without one (the Azure spare's, #308) Azure manages its
// network, so there is no load balancer or public IP of ours to pay for, and
// the apps reach the data accounts over their public endpoints with Entra ID
// (ADR-0008, amendment "No VNet for the spare"). Either way it has the Consumption
// profile: no Dedicated profile, so no management fee, and replicas can
// reach 4 vCPU / 8 GiB (08 §8.1). For a full index rebuild, the writer needs
// more memory than that (#172): `dedicatedProfile` adds a memory-optimized
// E4 profile (4 vCPU / 32 GiB, no minimum nodes) that the ingest job can run
// on. While it exists the environment pays the Dedicated plan management fee,
// so it's turned on for a rebuild and off again afterwards.

param location string
param tags object
param name string
@description('The resource group Azure creates for the environment\'s infrastructure (create-only); empty leaves Azure\'s ME_{name}_{group}_{region}.')
param infrastructureResourceGroup string = ''
@description('The delegated subnet for VNet integration; empty for an environment without a VNet.')
param subnetId string = ''
@description('Add the memory-optimized E4 profile the ingest job uses for a full rebuild (#172).')
param dedicatedProfile bool = false

// The ingest job's profile when `dedicatedProfile` is on.
var dedicatedName = 'ingest-e4'

resource env 'Microsoft.App/managedEnvironments@2024-03-01' = {
  name: name
  location: location
  tags: tags
  // The infrastructure group can only be set at creation: the existing
  // environments were created without one, so it is set only when not empty.
  properties: union(
    {
      workloadProfiles: concat(
        [{ name: 'Consumption', workloadProfileType: 'Consumption' }],
        dedicatedProfile
          ? [{ name: dedicatedName, workloadProfileType: 'E4', minimumCount: 0, maximumCount: 1 }]
          : []
      )
      // Logs go through the environment's diagnostic setting
      // (diagnostics.bicep), not the workspace's shared key.
      appLogsConfiguration: {
        destination: 'azure-monitor'
      }
      zoneRedundant: false
    },
    empty(subnetId) ? {} : { vnetConfiguration: { infrastructureSubnetId: subnetId, internal: false } },
    // Azure creates the infrastructure group only for a VNet-integrated environment.
    empty(infrastructureResourceGroup) || empty(subnetId) ? {} : { infrastructureResourceGroup: infrastructureResourceGroup }
  )
}

output id string = env.id
output name string = env.name
// The E4 profile's name, or empty when there is none.
output dedicatedProfileName string = dedicatedProfile ? dedicatedName : ''
output defaultDomain string = env.properties.defaultDomain
// The apex A record's target (an apex can't be a CNAME).
output staticIp string = env.properties.staticIp
// The value of the `asuid.{name}` TXT record that proves a custom domain.
output customDomainVerificationId string = env.properties.customDomainConfiguration.customDomainVerificationId
