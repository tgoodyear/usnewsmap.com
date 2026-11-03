// Existing OCR Syslog workspace and environment BOINC/OCR action group only.
param location string
param tags object = {}
@minLength(4)
param workspaceName string
@minLength(1)
param actionGroupId string
@description('Exact existing Arc/Syslog Computer identity of the evaluator. Private deployment input.')
@minLength(1)
param computer string

resource workspace 'Microsoft.OperationalInsights/workspaces@2023-09-01' existing = {
  name: workspaceName
}

var latest = '''
let FeedEvents = Syslog
| where TimeGenerated > ago(10m)
| where Computer == '__COMPUTER__' and ProcessName == 'usnm-boinc-feed-health'
| extend Feed = parse_json(SyslogMessage)
| where Feed.event == 'boinc_feed_health' and Feed.schema_version == 1
| extend Observed = unixtime_seconds_todatetime(tolong(Feed.observed_at));
FeedEvents
| summarize arg_max(TimeGenerated, *)
'''
var queries = [
  {
    name: 'condition'
    query: '''
| where Observed between (ago(3m) .. now())
| mv-expand Check = Feed.checks
| where tostring(Check.state) in ('firing', 'unknown')
| project Check = tostring(Check.code)
| union (
    FeedEvents
    | where Observed between (ago(10m) .. now())
    | mv-expand Check = Feed.checks
    | where Check.state == 'firing'
    | project Check = tostring(Check.code)
)
| distinct Check
'''
  }
  {
    name: 'missing'
    query: '''
| summarize Fresh = countif(Observed between (ago(5m) .. now()))
| where Fresh == 0
| project Check = 'collector_heartbeat'
'''
  }
]

resource alerts 'Microsoft.Insights/scheduledQueryRules@2023-12-01' = [for rule in queries: {
  name: '${workspaceName}-boinc-feed-${rule.name}'
  location: location
  tags: tags
  kind: 'LogAlert'
  properties: {
    displayName: 'BOINC feed ${rule.name}'
    description: 'Read-only BOINC feed health (#1441). Check the OCR dashboard BOINC feed / stale data panel. Do not start a second feed owner or automatically renew admission windows.'
    enabled: true
    severity: 2
    evaluationFrequency: 'PT5M'
    windowSize: 'PT10M'
    scopes: [workspace.id]
    skipQueryValidation: true
    autoMitigate: true
    criteria: {
      allOf: [{
        query: '${replace(latest, '__COMPUTER__', replace(computer, '\'', '\\\''))}${rule.query}'
        timeAggregation: 'Count'
        operator: 'GreaterThan'
        threshold: 0
        dimensions: [{ name: 'Check', operator: 'Include', values: ['*'] }]
        failingPeriods: { numberOfEvaluationPeriods: 1, minFailingPeriodsToAlert: 1 }
      }]
    }
    actions: { actionGroups: [actionGroupId] }
  }
}]
