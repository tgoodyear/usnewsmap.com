// Alerts on the on-prem OCR pickup (#1139): `usnm-ingest ocr-sync`, hourly at
// minute 17 on pve-laptop CT 215 (usnm-batch, usnm-ocr-sync.timer), in place of
// the Azure job usnm-ocrs-caj-<env> whose alerts (ingest-alerts.bicep) read its
// console logs. The run's JSON lines stay in the container's journal; what reaches
// Azure is the metrics it exports to the environment's Application Insights
// component with the container's Arc identity (Monitoring Metrics Publisher,
// ocr-sync-runner.bicep). So these are log search alerts on the environment's
// workspace, over AppMetrics:
// - ingest.command_failures (command ocr-sync): every usnm-ingest that ends in
//   error records one before it exits (crates/usnm-ingest/src/main.rs);
// - ocr.out.oldest_age_s and ocr.out.parts_left: recorded once per run that
//   got through the pickup, when it writes reference/status/ocr-local.json
//   (crates/usnm-ocr-local/src/sync.rs).
// No agent on the container and no new table: the rows are the ones the run
// already sends, a few a run.
//
// Rules, email through the environment's action group, stateful (one email when
// a condition starts, resolved when it clears), each $0.50 a month at 15 minutes
// or more (#308):
// - failed (severity 2): a run recorded a failure in the last 30 minutes;
// - stale (severity 2): no run has got through the pickup for more than 2 hours
//   (2 h 20 min: two missed hourly runs, and time for the third to finish and
//   its metrics to arrive). It also covers what the failure count can't: a run
//   killed before it records anything (out of memory, the unit's timeout), the
//   timer stopped, the container down, or the telemetry not getting through;
// - backlog (severity 3): the last run left a part in ocr-out older than 72
//   hours (ocr.out.parts_left says how many, in the query results). ocr-out deletes blobs 30 days after their last change.
// Each rule splits by one column, Detail, so the email carries it (the common
// schema lists a log alert's dimensions, not its other columns). Detail stays
// the same while a condition lasts: the last failure's time (a new failure is a
// new alert), the last good run's time (for two days, the stale rule's window), the oldest
// waiting part's day. Counts
// and ages, which change at every check, stay out of it; the alert's query
// results link has them.
//
// Every query reads only the environment component's rows (_ResourceId) under
// the cloud role usnm-ingest, as api-alerts.bicep does. Anyone with Monitoring
// Metrics Publisher on the component can write rows under any role, so a host
// with that role could raise or quiet these alerts; that is the same trust the
// API alerts place in it.

param location string
param tags object
param workspaceId string
@description('The environment\'s Application Insights component, where the pickup sends its metrics.')
param appInsightsId string
param actionGroupId string
@description('The alert rules\' names (infra/naming.bicep): failed, stale and backlog.')
param alertNames object

var failed = '''
AppMetrics
| where _ResourceId =~ "__COMPONENT_ID__" and AppRoleName == "usnm-ingest"
| where Name == "ingest.command_failures" and tostring(Properties["command"]) == "ocr-sync"
| where TimeGenerated > ago(30m) and Sum > 0
| summarize Last = max(TimeGenerated)
| where isnotnull(Last)
| project Detail = strcat("the last failed run ended ", format_datetime(Last, "yyyy-MM-dd HH:mm"), " UTC")
'''

var stale = '''
let lastRun = toscalar(AppMetrics
    | where _ResourceId =~ "__COMPONENT_ID__" and AppRoleName == "usnm-ingest"
    | where Name == "ocr.out.oldest_age_s"
    | summarize max(TimeGenerated));
print Last = lastRun
| where isnull(Last) or Last < ago(140m)
| project Detail = iff(isnull(Last), "no run got through the pickup in the last 2 days",
    strcat("the last run that got through the pickup ended ", format_datetime(Last, "yyyy-MM-dd HH:mm"), " UTC"))
'''

var backlog = '''
AppMetrics
| where _ResourceId =~ "__COMPONENT_ID__" and AppRoleName == "usnm-ingest"
| where Name == "ocr.out.oldest_age_s"
| summarize arg_max(TimeGenerated, OldestS = Max)
| where OldestS > 72 * 3600
// The oldest part's day, which stays the same while that part waits.
| project Detail = strcat("parts waiting since ", format_datetime(bin(TimeGenerated - OldestS * 1s, 1d), "yyyy-MM-dd"), " UTC")
'''

var rules = [
  {
    name: alertNames.failed
    displayName: 'OCR pickup failed'
    description: 'A run of the on-prem OCR pickup (usnm-ingest ocr-sync, usnm-ocr-sync.service in pve-laptop CT 215) ended in error in the last 30 minutes; Detail says when the last one ended. Nothing it checked is lost: the next hourly run picks up where this one stopped. Next: in CT 215, journalctl -u usnm-ocr-sync --since -2h for the "command failed" line (the whole error chain), and reference/status/ocr-local.json for the last run that got through.'
    severity: 2
    frequency: 'PT15M'
    // The query reports the last 30 minutes (two checks), so a row that arrives a few minutes late is still seen.
    window: 'PT30M'
    query: failed
    dimensions: ['Detail']
  }
  {
    name: alertNames.stale
    displayName: 'OCR pickup not running'
    description: 'No run of the on-prem OCR pickup (usnm-ingest ocr-sync, hourly at minute 17 in pve-laptop CT 215) has got through for more than 2 hours; Detail says when the last one did. Node output waits in ocr-out meanwhile. Next: in CT 215, systemctl status usnm-ocr-sync.timer usnm-ocr-sync.service and journalctl -u usnm-ocr-sync --since -3h; on pve-laptop, home-infra scripts/lab/batch-ct-ingest.sh --check. If the runs succeed but this stays on, their telemetry isn\'t reaching Application Insights (APPLICATIONINSIGHTS_CONNECTION_STRING in /etc/usnm/ingest.env, the Arc agent).'
    severity: 2
    frequency: 'PT15M'
    // Two days, the longest a log alert reads, so the last good run stays in view (and
    // Detail stays the same) through an outage; one that lasts longer emails again once.
    window: 'P2D'
    query: stale
    dimensions: ['Detail']
  }
  {
    name: alertNames.backlog
    displayName: 'OCR output waiting over 72 hours'
    description: 'The on-prem OCR pickup\'s last run left node output in ocr-out that is more than 72 hours old; Detail says since when; the query results link has how many parts and how old. ocr-out deletes blobs 30 days after their last change. Next: reference/status/ocr-local.json (parts_left, oldest_age_s) and, in CT 215, journalctl -u usnm-ocr-sync; raise --max-parts, or start extra runs with usnm-ingest-run ocr-sync.'
    severity: 3
    frequency: 'PT1H'
    window: 'PT3H'
    query: backlog
    dimensions: ['Detail']
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
      severity: r.severity
      enabled: true
      evaluationFrequency: r.frequency
      windowSize: r.window
      scopes: [workspaceId]
      // A workspace has no AppMetrics table until something sends metrics.
      skipQueryValidation: true
      autoMitigate: true
      criteria: {
        allOf: [
          {
            query: replace(r.query, '__COMPONENT_ID__', appInsightsId)
            timeAggregation: 'Count'
            operator: 'GreaterThan'
            threshold: 0
            dimensions: [for d in r.dimensions: { name: d, operator: 'Include', values: ['*'] }]
            failingPeriods: {
              numberOfEvaluationPeriods: 1
              minFailingPeriodsToAlert: 1
            }
          }
        ]
      }
      actions: { actionGroups: [actionGroupId] }
    }
  }
]
