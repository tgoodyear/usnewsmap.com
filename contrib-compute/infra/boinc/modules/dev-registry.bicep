// No production-registry grants: dev imports reviewed artifacts into its own registry.
param name string
param location string

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' = {
  name: name
  location: location
  sku: { name: 'Basic' }
  properties: { adminUserEnabled: false, publicNetworkAccess: 'Enabled' }
}

output name string = registry.name
