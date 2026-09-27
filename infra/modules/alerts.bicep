// Alert whenever a data account is changed at the control plane, which
// includes flipping publicNetworkAccess for a backfill window (08 §8.3).

param name string
param actionGroupId string
param storageId string
param cosmosId string

resource alert 'Microsoft.Insights/activityLogAlerts@2020-10-01' = {
  name: name
  location: 'global'
  properties: {
    enabled: true
    scopes: [resourceGroup().id]
    condition: {
      allOf: [
        { field: 'category', equals: 'Administrative' }
        {
          anyOf: [
            { field: 'resourceId', equals: storageId }
            { field: 'resourceId', equals: cosmosId }
          ]
        }
        {
          anyOf: [
            { field: 'operationName', equals: 'Microsoft.Storage/storageAccounts/write' }
            { field: 'operationName', equals: 'Microsoft.DocumentDB/databaseAccounts/write' }
          ]
        }
      ]
    }
    actions: { actionGroups: [{ actionGroupId: actionGroupId }] }
  }
}
