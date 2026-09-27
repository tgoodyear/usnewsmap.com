// User-assigned managed identities (08 §8.2). The launcher identity arrives
// with the backfill launcher and network-guard jobs.

param location string
param tags object
param appName string
param ingestName string

resource app 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: appName
  location: location
  tags: tags
}

resource ingest 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: ingestName
  location: location
  tags: tags
}

output appId string = app.id
output appClientId string = app.properties.clientId
output appPrincipalId string = app.properties.principalId
output ingestId string = ingest.id
output ingestClientId string = ingest.properties.clientId
output ingestPrincipalId string = ingest.properties.principalId
