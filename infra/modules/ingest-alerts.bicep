// Log search alerts on the ingest and backfill jobs (09 §9.2), evaluated on
// the Log Analytics workspace against the resource-specific Container Apps
// tables (ContainerAppConsoleLogs, ContainerAppSystemLogs). Severity 2,
// email through the environment's action group. Stateful: one notification
// when a condition starts, resolved when it clears.
//
// The queries read the JSON lines usnm-ingest writes ("command failed",
// "building index", "release progress", "published", "curated"); change
// them together with the log messages. Lines that could carry the managed
// identity endpoint's secret are dropped first.

param location string
param tags object
param nameSuffix string
param workspaceId string
param actionGroupId string

// Every error exit of usnm-ingest ends with one "command failed" line: the
// reliable signal, with the error text. A replica killed without writing it
// (out of memory, the 24 h replica timeout, a failed image pull) shows only
// in the platform's events: the job's backoff or deadline, or a container
// that exited non-zero.
var jobFailed = '''
let console = ContainerAppConsoleLogs
    | where ContainerName in ("ingest", "curate")
    | where Log !has "IDENTITY_HEADER" and Log !has "MSI_SECRET"
    | where Log has "failed"
    | extend j = parse_json(Log)
    | where tostring(j.level) == "ERROR" and tostring(j.fields.message) == "command failed"
    | project TimeGenerated, Job = coalesce(ContainerAppName, JobName), Replica = ContainerGroupName,
        Detail = substring(tostring(j.fields.error), 0, 500);
let platform = ContainerAppSystemLogs
    | where JobName startswith "caj-usnm-" or ContainerAppName startswith "caj-usnm-"
    | where Log !has "IDENTITY_HEADER" and Log !has "MSI_SECRET"
    | where Reason in ("BackoffLimitExceeded", "DeadlineExceeded")
        or (Log contains "terminated with exit code" and Log !contains "exit code '0'")
    | project TimeGenerated, Job = coalesce(JobName, ContainerAppName), Replica = ReplicaName,
        Detail = substring(strcat(Reason, ": ", Log), 0, 500);
union console, platform
'''

// A release that started ("building index") and hasn't ended ("published"
// or "command failed") but has logged no "release progress" line for 10
// minutes: the job hung, or died without a word. Progress is logged every
// 30 s while the index builds.
var releaseStalled = '''
let lines = ContainerAppConsoleLogs
    | where ContainerName == "ingest"
    | where Log !has "IDENTITY_HEADER" and Log !has "MSI_SECRET"
    | where Log has_any ("building", "progress", "published", "failed")
    | extend Message = tostring(parse_json(Log).fields.message), Replica = ContainerGroupName;
let started = lines | where Message == "building index" | summarize Started = max(TimeGenerated) by Replica;
let ended = lines | where Message in ("published", "command failed") | summarize Ended = max(TimeGenerated) by Replica;
let progress = lines | where Message == "release progress" | summarize LastProgress = max(TimeGenerated) by Replica;
started
| join kind=leftouter ended on Replica
| join kind=leftouter progress on Replica
| where isnull(Ended) or Ended < Started
| where Started < ago(10m)
| where isnull(LastProgress) or LastProgress < ago(10m)
| project Replica, Started, LastProgress
'''

// Backfill workers are running (logged in the last 15 minutes) but none has
// curated a batch in an hour. Not while LoC's rate limit holds downloads
// (a "throttled" line: every worker waits an hour).
var backfillStalled = '''
let logs = ContainerAppConsoleLogs
    | where ContainerName == "curate"
    | where Log !has "IDENTITY_HEADER" and Log !has "MSI_SECRET"
    | extend Message = tostring(parse_json(Log).fields.message);
let active = toscalar(logs | where TimeGenerated > ago(15m) | count);
let curated = toscalar(logs | where TimeGenerated > ago(60m) | where Message == "curated" | count);
let throttled = toscalar(logs | where TimeGenerated > ago(75m) | where Message startswith "throttled" | count);
print Active = active, Curated = curated, Throttled = throttled
| where Active > 0 and Curated == 0 and Throttled == 0
'''

var rules = [
  {
    name: 'ingest-job-failed'
    displayName: 'Ingest or backfill job failed'
    description: 'An ingest or backfill replica exited with an error in the last 15 minutes. scripts/logs.sh <env> job-executions and errors-by-batch show which and why.'
    frequency: 'PT5M'
    window: 'PT15M'
    query: jobFailed
  }
  {
    name: 'release-stalled'
    displayName: 'Release stalled'
    description: 'A release is building an index but has logged no progress for 10 minutes. scripts/logs.sh <env> release-progress shows the last lines.'
    frequency: 'PT10M'
    window: 'PT3H'
    query: releaseStalled
  }
  {
    name: 'backfill-stalled'
    displayName: 'Backfill stalled'
    description: 'Backfill workers are running but none has curated a batch in an hour (and LoC is not rate limiting). scripts/logs.sh <env> curation-throughput and errors-by-batch show the recent work.'
    frequency: 'PT15M'
    window: 'PT2H'
    query: backfillStalled
  }
]

resource alert 'Microsoft.Insights/scheduledQueryRules@2023-12-01' = [
  for r in rules: {
    name: 'alert-usnm-${r.name}-${nameSuffix}'
    location: location
    tags: tags
    kind: 'LogAlert'
    properties: {
      displayName: r.displayName
      description: r.description
      severity: 2
      enabled: true
      evaluationFrequency: r.frequency
      windowSize: r.window
      scopes: [workspaceId]
      // A new workspace has no Container Apps tables until the first logs
      // arrive; the rules start working once they do.
      skipQueryValidation: true
      autoMitigate: true
      criteria: {
        allOf: [
          {
            query: r.query
            timeAggregation: 'Count'
            operator: 'GreaterThan'
            threshold: 0
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
