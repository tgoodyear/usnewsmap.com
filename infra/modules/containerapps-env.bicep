// VNet-integrated workload-profiles environment using only the Consumption
// profile: no Dedicated profile, so no management fee, and replicas can
// reach 4 vCPU / 8 GiB (08 §8.1).

param location string
param tags object
param name string
param subnetId string

resource env 'Microsoft.App/managedEnvironments@2024-03-01' = {
  name: name
  location: location
  tags: tags
  properties: {
    workloadProfiles: [
      { name: 'Consumption', workloadProfileType: 'Consumption' }
    ]
    vnetConfiguration: {
      infrastructureSubnetId: subnetId
      internal: false
    }
    // Logs go through the environment's diagnostic setting
    // (diagnostics.bicep), not the workspace's shared key.
    appLogsConfiguration: {
      destination: 'azure-monitor'
    }
    zoneRedundant: false
  }
}

output id string = env.id
output name string = env.name
output defaultDomain string = env.properties.defaultDomain
// The apex A record's target (an apex can't be a CNAME).
output staticIp string = env.properties.staticIp
// The value of the `asuid.{name}` TXT record that proves a custom domain.
output customDomainVerificationId string = env.properties.customDomainConfiguration.customDomainVerificationId
