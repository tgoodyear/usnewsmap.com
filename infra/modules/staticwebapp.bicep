// SPA hosting (08 §8.1, ADR-0006): Free tier replaces Front Door for the site.
// Content is deployed by CI with the SWA deployment token; no repo link here.

param location string
param tags object
param name string
@allowed(['Free', 'Standard'])
param sku string = 'Free'

resource swa 'Microsoft.Web/staticSites@2023-12-01' = {
  name: name
  location: location
  tags: tags
  sku: { name: sku, tier: sku }
  properties: {
    stagingEnvironmentPolicy: 'Enabled'
    allowConfigFileUpdates: true
  }
}

output id string = swa.id
output defaultHostname string = swa.properties.defaultHostname
