// Native on-prem Japanese runs (#1164), independent of retired Azure ingest jobs.
// The existing supervisor uses CT215's existing Entra telemetry grant, not a new
// grant to all on-prem hosts. Freshness is explicitly armed only for planned runs.
param location string
param tags object
param workspaceId string
param appInsightsId string
param actionGroupId string
param alertNames object
@description('An operator currently expects an active Japanese run. Disarm for intentional idle.')
param expectedActive bool = false

var failed = '''
AppMetrics
| where _ResourceId =~ "__COMPONENT_ID__" and AppRoleName == "usnm-ingest"
| where Name == "ingest.command_failures" and tostring(Properties["command"]) == "ja-ocr-run"
| where TimeGenerated > ago(30m) and Sum > 0
| summarize Last = max(TimeGenerated)
| where isnotnull(Last)
| project Detail = strcat("the last supervised Japanese run failed ", format_datetime(Last, "yyyy-MM-dd HH:mm"), " UTC")
'''

var stale = '''
let lastRun = toscalar(AppMetrics
    | where _ResourceId =~ "__COMPONENT_ID__" and AppRoleName == "usnm-ingest"
    | where Name == "jaocr.run_heartbeat"
    | summarize max(TimeGenerated));
print Last = lastRun
| where isnull(Last) or Last < ago(30m)
| project Detail = iff(isnull(Last), "an active Japanese run is expected but no heartbeat arrived in 2 days",
    strcat("an active Japanese run is expected; last heartbeat ", format_datetime(Last, "yyyy-MM-dd HH:mm"), " UTC"))
'''

var rules = [
  {
    name: alertNames.failed
    displayName: 'On-prem Japanese OCR failed'
    description: 'A supervised on-prem Japanese OCR invocation failed. Read the local usnm-ja-ocr-native journal; child output is not exported to Application Insights. Check the runner, Arc identity and source-rate holds before retrying.'
    enabled: true
    window: 'PT30M'
    query: failed
  }
  {
    name: alertNames.stale
    displayName: 'Expected Japanese OCR heartbeat missing'
    description: 'USNM_JA_OCR_EXPECT_ACTIVE is armed but no active-run heartbeat arrived for 30 minutes. Check the native unit, host and telemetry. Intentional idle must disarm this setting; an old run or USNM_JA_OCR_ONPREM alone never implies expected activity.'
    enabled: expectedActive
    window: 'P2D'
    query: stale
  }
]

resource alert 'Microsoft.Insights/scheduledQueryRules@2023-12-01' = [
  for r in rules: {
    name: r.name
    location: location
    tags: tags
    kind: 'LogAlert'
    properties: {
      displayName: r.displayName
      description: r.description
      severity: 2
      enabled: r.enabled
      evaluationFrequency: 'PT15M'
      windowSize: r.window
      scopes: [workspaceId]
      skipQueryValidation: true
      autoMitigate: true
      criteria: {
        allOf: [
          {
            query: replace(r.query, '__COMPONENT_ID__', appInsightsId)
            timeAggregation: 'Count'
            operator: 'GreaterThan'
            threshold: 0
            dimensions: [{ name: 'Detail', operator: 'Include', values: ['*'] }]
            failingPeriods: { numberOfEvaluationPeriods: 1, minFailingPeriodsToAlert: 1 }
          }
        ]
      }
      actions: { actionGroups: [actionGroupId] }
    }
  }
]
