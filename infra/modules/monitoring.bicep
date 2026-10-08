// Log Analytics with a daily cap (a ceiling on bursts; normal days stay within the free 5 GB/month) and
// workspace-based Application Insights (08 §8.1). Optional action group.

param location string
param tags object
param workspaceName string
param appInsightsName string
param actionGroupName string
@description('Alert recipients; no action group is created when empty.')
param alertEmails array = []
// 0.15 GB (inside the free 5 GB/month at any rate) stopped all logging on 2026-10-07: the
// American Stories writer's blob writes reached it by 17:46 ET, and nothing was logged, alerts
// included, until the next reset. 1 GB leaves room for bulk jobs; a normal day is about 0.08 GB.
@description('Daily ingestion cap in GB.')
param dailyCapGb string = '1'

resource workspace 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: workspaceName
  location: location
  tags: tags
  properties: {
    sku: { name: 'PerGB2018' }
    retentionInDays: 30
    workspaceCapping: { dailyQuotaGb: json(dailyCapGb) }
    // Entra only (ADR-0009): no ingestion or queries with the workspace keys.
    features: { disableLocalAuth: true }
  }
}

resource appInsights 'Microsoft.Insights/components@2020-02-02' = {
  name: appInsightsName
  location: location
  tags: tags
  kind: 'web'
  properties: {
    Application_Type: 'web'
    WorkspaceResourceId: workspace.id
    DisableIpMasking: false
    // Entra only (ADR-0009): telemetry must be sent with a managed identity
    // (Monitoring Metrics Publisher), not the instrumentation key.
    DisableLocalAuth: true
  }
}

resource actionGroup 'Microsoft.Insights/actionGroups@2023-01-01' = if (!empty(alertEmails)) {
  name: actionGroupName
  location: 'global'
  tags: tags
  properties: {
    groupShortName: 'usnm'
    enabled: true
    emailReceivers: [
      for (email, i) in alertEmails: {
        name: 'email-${i}'
        emailAddress: email
        useCommonAlertSchema: true
      }
    ]
  }
}

output workspaceId string = workspace.id
output workspaceName string = workspace.name
// The workspace's id in the Log Analytics query API (scripts/logs.sh).
output workspaceCustomerId string = workspace.properties.customerId
output appInsightsName string = appInsights.name
output appInsightsId string = appInsights.id
// The ingestion endpoint and instrumentation key, not a credential: with
// local auth disabled, ingestion needs an Entra token as well.
output appInsightsConnectionString string = appInsights.properties.ConnectionString
output actionGroupId string = empty(alertEmails) ? '' : actionGroup.id
