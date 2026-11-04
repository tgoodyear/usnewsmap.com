// Alerts on the live BOINC feed health (#1441), through the environment's
// existing action group (monitoring.bicep). Deployed by main.bicep with
// boincFeedAlerts (USNM_BOINC_FEED_ALERTS), ocrNodes and alert emails.
//
// The evaluator (scripts/boinc/feed_status.py, usnm-boinc-feed-health.service)
// prints one JSON document a minute; its unit logs it to local0 at Info as
// usnm-boinc-feed-health. The evaluator host's Arc machine (machineName, in
// the environment's resource group; a private setting) is associated with
// the rule below by hand, as onboard.sh associates the nodes with theirs.
// The rule collects only local0 at Info and above into the OCR nodes'
// workspace: no counters or system facilities, about 2.6 MB a day (one line
// of about 1.8 KB a minute with five own clients). The nodes' rule would also
// send the host's counters and VM insights, 20 to 45 MB a day, against the
// workspace's 0.1 GB cap.
//
// Every query reads only that machine's rows (_ResourceId, which the agent
// sets from the machine's identity; a node could fake Computer, not that).
// Two stateful rules, split by Check, so Azure sends one email when a check
// starts and a resolved one when it clears; the evaluator sends no
// transitions of its own:
// - condition: a check firing or unknown in the latest event of the last 3
//   minutes, or firing in any event of the last 10 (a shortage that recovers
//   between two evaluations still alerts). Also Check = delivery_test for a
//   boinc_feed_delivery_test line in the last 10 minutes
//   (feed_status.py --delivery-test), the end-to-end notification check.
//   That line never counts as a health event: it can't change the latest
//   state or keep the missing rule quiet.
// - missing: no health event observed in the last 5 minutes
//   (Check = collector_heartbeat): the evaluator stopped, its timer, the
//   host, the agent or the association is gone, or the cap was reached.
param location string
param tags object = {}
@minLength(4)
param workspaceName string
@minLength(1)
param actionGroupId string
@description('Name of the evaluator host\'s Arc machine in this resource group. Private deployment input.')
@minLength(1)
param machineName string
@description('The data collection rule\'s name (naming.bicep).')
param ruleName string
@description('The alert rules\' names (naming.bicep): condition and missing.')
param alertNames object

resource workspace 'Microsoft.OperationalInsights/workspaces@2023-09-01' existing = {
  name: workspaceName
}

var infoAndAbove = ['Info', 'Notice', 'Warning', 'Error', 'Critical', 'Alert', 'Emergency']

resource dcr 'Microsoft.Insights/dataCollectionRules@2023-03-11' = {
  name: ruleName
  location: location
  tags: tags
  kind: 'Linux'
  properties: {
    description: 'BOINC feed health evaluator (#1441): local0 at Info and above (usnm-boinc-feed-health), into ${workspaceName}. Associate only the evaluator host.'
    dataSources: {
      syslog: [
        {
          name: 'feedHealth'
          streams: ['Microsoft-Syslog']
          facilityNames: ['local0']
          logLevels: infoAndAbove
        }
      ]
    }
    destinations: {
      logAnalytics: [
        {
          name: 'workspace'
          workspaceResourceId: workspace.id
        }
      ]
    }
    dataFlows: [
      {
        streams: ['Microsoft-Syslog']
        destinations: ['workspace']
      }
    ]
  }
}

// The empty fallback keeps the missing rule working before the workspace
// has a Syslog table: no table counts as no health events, not an error.
var lines = '''
let FeedLines = union isfuzzy=true
    (Syslog
    | where TimeGenerated > ago(10m)
    | where _ResourceId =~ '__MACHINE_ID__' and ProcessName == 'usnm-boinc-feed-health'
    | project TimeGenerated, SyslogMessage),
    (datatable(TimeGenerated: datetime, SyslogMessage: string)[])
| extend Feed = parse_json(SyslogMessage)
| where Feed.schema_version == 1
| extend Observed = unixtime_seconds_todatetime(tolong(Feed.observed_at));
let FeedEvents = FeedLines
| where Feed.event == 'boinc_feed_health';
FeedEvents
| summarize arg_max(TimeGenerated, *)
'''
var queries = [
  {
    name: alertNames.condition
    displayName: 'BOINC feed condition'
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
| union (
    FeedLines
    | where Feed.event == 'boinc_feed_delivery_test' and tobool(Feed.test)
    | where Observed between (ago(10m) .. now())
    | project Check = 'delivery_test'
    | distinct Check
)
'''
  }
  {
    name: alertNames.missing
    displayName: 'BOINC feed missing'
    query: '''
| summarize Fresh = countif(Observed between (ago(5m) .. now()))
| where Fresh == 0
| project Check = 'collector_heartbeat'
'''
  }
]

var machineId = resourceId('Microsoft.HybridCompute/machines', machineName)

resource alerts 'Microsoft.Insights/scheduledQueryRules@2023-12-01' = [for alert in queries: {
  name: alert.name
  location: location
  tags: tags
  kind: 'LogAlert'
  properties: {
    displayName: alert.displayName
    description: 'Read-only BOINC feed health (#1441). Check names the check; delivery_test is the end-to-end notification check (docs/boinc/boundaries-and-monitoring.md). Check the OCR dashboard BOINC feed / stale data panel. Do not start a second feed owner or automatically renew admission windows.'
    enabled: true
    severity: 2
    evaluationFrequency: 'PT5M'
    windowSize: 'PT10M'
    scopes: [workspace.id]
    // No Syslog table until the first line arrives.
    skipQueryValidation: true
    autoMitigate: true
    criteria: {
      allOf: [{
        query: '${replace(lines, '__MACHINE_ID__', replace(machineId, '\'', '\\\''))}${alert.query}'
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

output ruleId string = dcr.id
