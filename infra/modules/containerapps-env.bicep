// VNet-integrated workload-profiles environment using only the Consumption
// profile: no Dedicated profile, so no management fee, and replicas can
// reach 4 vCPU / 8 GiB (08 §8.1).

param location string
param tags object
param name string
param subnetId string
param workspaceName string

resource workspace 'Microsoft.OperationalInsights/workspaces@2023-09-01' existing = {
  name: workspaceName
}

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
    appLogsConfiguration: {
      destination: 'log-analytics'
      logAnalyticsConfiguration: {
        customerId: workspace.properties.customerId
        sharedKey: workspace.listKeys().primarySharedKey
      }
    }
    zoneRedundant: false
  }
}

output id string = env.id
output defaultDomain string = env.properties.defaultDomain
// The value of the `asuid.{name}` TXT record that proves a custom domain.
output customDomainVerificationId string = env.properties.customDomainConfiguration.customDomainVerificationId
