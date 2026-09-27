// Log Analytics with a daily cap (stays within the free 5 GB/month) and
// workspace-based Application Insights (08 §8.1). Optional action group.

param location string
param tags object
param workspaceName string
param appInsightsName string
param actionGroupName string
@description('Alert recipients; no action group is created when empty.')
param alertEmails array = []
@description('Daily ingestion cap in GB (0.15 ≈ 4.5 GB/month).')
param dailyCapGb string = '0.15'

resource workspace 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: workspaceName
  location: location
  tags: tags
  properties: {
    sku: { name: 'PerGB2018' }
    retentionInDays: 30
    workspaceCapping: { dailyQuotaGb: json(dailyCapGb) }
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
output appInsightsConnectionString string = appInsights.properties.ConnectionString
output actionGroupId string = empty(alertEmails) ? '' : actionGroup.id
