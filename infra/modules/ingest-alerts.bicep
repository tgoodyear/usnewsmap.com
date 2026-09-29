// Log search alerts on the ingest and backfill jobs (09 §9.2), evaluated on
// the Log Analytics workspace against the resource-specific Container Apps
// tables (ContainerAppConsoleLogs, ContainerAppSystemLogs). Severity 2,
// email through the environment's action group. Stateful: one notification
// when a condition starts, resolved when it clears.
//
// The queries read the JSON lines usnm-ingest writes ("command failed",
// "building index", "release progress", "published", "curated", "claimed",
// "curate progress", "curation finished"); change them together with the
// log messages. Lines that could carry the managed
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
// 30 s while the index builds. The window is a day, the job's replica
// timeout: a full release can take most of it (04 §4.1.1).
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

// Backfill workers have been running for the last hour (they logged in its
// first 15 minutes and in its last 15) but none has curated a batch in it.
// Not while LoC's rate limit holds downloads (a "throttled" line: every
// worker waits an hour). Workers that keep logging "curate progress" without
// finishing a batch are caught here; one that stops logging is caught by
// backfillReplicaSilent.
var backfillStalled = '''
let logs = ContainerAppConsoleLogs
    | where ContainerName == "curate"
    | where Log !has "IDENTITY_HEADER" and Log !has "MSI_SECRET"
    | extend Message = tostring(parse_json(Log).fields.message);
let early = toscalar(logs | where TimeGenerated between (ago(60m) .. ago(45m)) | count);
let late = toscalar(logs | where TimeGenerated > ago(15m) | count);
let curated = toscalar(logs | where TimeGenerated > ago(60m) | where Message == "curated" | count);
let throttled = toscalar(logs | where TimeGenerated > ago(75m) | where Message startswith "throttled" | count);
print Early = early, Late = late, Curated = curated, Throttled = throttled
| where Early > 0 and Late > 0 and Curated == 0 and Throttled == 0
'''

// One backfill replica has logged nothing for 15 minutes, although it hasn't
// ended: no "curation finished" or "command failed" line, no platform event
// that stopped the replica, and no stop of its execution. Job-level stop
// events don't name the execution, so each is matched to the latest
// execution that started before it (the replica name is the execution's
// name plus a suffix). A worker logs "curate progress" every minute while it
// holds a batch (and a batch it can't finish in 45 minutes is abandoned with
// a "curation timed out" line), so 15 minutes of silence means the process or
// its runtime is stuck, or its logs stopped arriving. Only replicas that have logged "claimed" are checked: images
// from before the heartbeat were silent for minutes by design. The window is
// a day, the job's replica timeout.
var backfillReplicaSilent = '''
let lines = ContainerAppConsoleLogs
    | where ContainerName == "curate"
    | where Log !has "IDENTITY_HEADER" and Log !has "MSI_SECRET"
    | extend Message = tostring(parse_json(Log).fields.message), Replica = ContainerGroupName;
let replicas = lines
    | summarize
        FirstLine = min(TimeGenerated),
        LastLine = max(TimeGenerated),
        Watched = countif(Message in ("claimed", "curate progress")),
        Ended = countif(Message in ("curation finished", "command failed"))
        by Replica
    | extend Execution = extract(@"^(.+)-[a-z0-9]+$", 1, Replica);
let platform = ContainerAppSystemLogs
    | where JobName startswith "caj-usnm-backfill-"
    | where Log !has "IDENTITY_HEADER" and Log !has "MSI_SECRET";
let replicaStops = platform
    | where Reason in ("ContainerTerminated", "PodDeletion", "ProcessExited", "SuccessfulDelete")
    | extend Replica = iff(isnotempty(ReplicaName), ReplicaName, extract(@"(caj-usnm-backfill-[a-z0-9-]+)", 1, Log))
    | summarize by Replica;
let executions = replicas | summarize Started = min(FirstLine) by Execution | extend k = 1;
let executionStops = platform
    | where Reason in ("Suspended", "DeadlineExceeded", "BackoffLimitExceeded")
    | project StopAt = TimeGenerated, k = 1
    | join kind=inner executions on k
    | where Started <= StopAt
    | summarize arg_max(Started, Execution) by StopAt
    | summarize by Execution;
replicas
| where Watched > 0 and Ended == 0
| where LastLine < ago(15m)
| join kind=leftanti replicaStops on Replica
| join kind=leftanti executionStops on Execution
| project Replica, LastLine, SilentMinutes = round((now() - LastLine) / 1m, 1)
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
    frequency: 'PT15M'
    window: 'P1D'
    query: releaseStalled
  }
  {
    name: 'backfill-stalled'
    displayName: 'Backfill stalled'
    description: 'Backfill workers have run for the last hour but none has curated a batch in it (and LoC is not rate limiting). scripts/logs.sh <env> curation-throughput and errors-by-batch show the recent work.'
    frequency: 'PT15M'
    window: 'PT2H'
    query: backfillStalled
  }
  {
    name: 'backfill-replica-silent'
    displayName: 'Backfill replica silent'
    description: 'A backfill replica that is still running has logged nothing for 15 minutes; a working one logs curate progress every minute. scripts/logs.sh <env> curate-replicas shows each replica\'s last stage and heartbeat.'
    frequency: 'PT5M'
    window: 'P1D'
    query: backfillReplicaSilent
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
