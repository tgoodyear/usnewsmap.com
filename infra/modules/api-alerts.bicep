// Alerts on the public API and site (09 §9.2), email through the
// environment's action group. Deployed with the API when alert emails are
// set.
//
// - API 5xx, aggregate latency and slow searches: log search alerts on the workspace,
//   reading what usnm-api exports to Application Insights: requests
//   (AppRequests, one row per request except the health probes; the
//   request name is "<method> <route template>") and, for slow searches,
//   the api.slow_searches metric (AppMetrics). Stateful: one
//   notification when a condition starts, resolved when it clears.
// - Availability: an Application Insights standard test of the site's home
//   page from three US locations, with a metric alert when two of the three
//   fail. The site and the API are one container app, so the home page
//   exercises DNS, TLS, ingress and the app; /readyz is already probed by
//   Container Apps and failing searches raise the 5xx alert. Only when the
//   site's hostname has a bound certificate (the test checks TLS). Results are written by the
//   availability service itself; the component's DisableLocalAuth only
//   restricts what clients send, and Microsoft's list of scenarios that
//   don't work with Entra-only ingestion doesn't include availability tests.
//   Each run is billed ($0.0005 per location per run in East US 2): every
//   15 minutes from 3 locations is about $4 a month.

param location string
param tags object
param nameSuffix string
param workspaceId string
param appInsightsId string
param actionGroupId string

@description('Site home page to test, e.g. https://usnewsmap.com/; empty skips the test.')
param siteUrl string = ''

// A string: the setting arrives as text, and only these values are valid
// for a standard test.
@description('Seconds between availability test runs from each location.')
@allowed(['300', '600', '900'])
param availabilityFrequency string = '900'

// At least 5 server errors in 10 minutes that are also more than 2% of the
// requests. The count floor keeps one or two failures on a quiet site from
// paging; the ratio keeps a handful among many requests from paging. Slow
// searches are left out: a search longer than a visitor waits gets a 202
// and carries on (06 §6.3.5), and the 503s for a search that ran past its
// 2-minute limit (/errors/backend-timeout) or found every computation slot
// taken (/errors/busy) are not counted. The request span names the problem
// type in usnm.problem. Slow searches have their own alert below.
var serverErrors = '''
AppRequests
| where AppRoleName == "usnm-api"
| extend Problem = tostring(Properties["usnm.problem"])
| summarize Requests = sum(ItemCount),
    Errors = sumif(ItemCount, toint(ResultCode) >= 500
        and Problem !in ("/errors/backend-timeout", "/errors/busy"))
| where Errors >= 5 and Errors * 50 > Requests
'''

// At least 3 searches in an hour took longer than a visitor waits (outcome
// ok, timeout or error, counted once per search however often its visitor
// asked again), or were refused because every computation slot was taken
// (busy). Cold searches after a new index version is published can trip it.
// Severity 3, checked every 15 minutes; it emails the same action group as
// the other alerts.
var slowSearches = '''
AppMetrics
| where AppRoleName == "usnm-api" and Name == "api.slow_searches"
| summarize Searches = sum(Sum)
| where Searches >= 3
'''

// p95 of /v1/aggregate above 3 s over 15 minutes, counting only answered
// requests (5xx have their own alert). A cold query takes 2-7 s and a warm
// one about 0.15 s, so a single cold search can push a small sample's p95
// over 3 s. It takes at least 20 requests and at least 3 slower than 3 s
// (so more than one or two cold searches) before this fires. Right after a
// new index version is published every search is cold, so a busy hour then
// can trip it; that is worth knowing but not urgent (severity 3).
var slowAggregate = '''
AppRequests
| where AppRoleName == "usnm-api"
| where Name in ("GET /v1/aggregate", "GET /api/v1/aggregate")
| where toint(ResultCode) < 500
| summarize Requests = sum(ItemCount), Slow = sumif(ItemCount, DurationMs > 3000),
    P95 = percentile(DurationMs, 95)
| where Requests >= 20 and Slow >= 3 and P95 > 3000
'''

var rules = [
  {
    name: 'api-server-errors'
    displayName: 'API server errors'
    description: 'usnm-api answered at least 5 requests with a 5xx in 10 minutes, more than 2% of its requests. scripts/logs.sh <env> api-errors shows which routes and codes.'
    severity: 2
    frequency: 'PT5M'
    window: 'PT10M'
    query: serverErrors
  }
  {
    name: 'api-aggregate-slow'
    displayName: 'Slow aggregate searches'
    description: 'p95 of /v1/aggregate was over 3 s in the last 15 minutes (at least 20 requests, 3 of them over 3 s). scripts/logs.sh <env> api-requests shows latency by route.'
    severity: 3
    frequency: 'PT5M'
    window: 'PT15M'
    query: slowAggregate
  }
  {
    name: 'api-searches-slow'
    displayName: 'Searches longer than a visitor waits'
    description: 'At least 3 searches in the last hour took longer than a visitor waits (USNM_SEARCH_TIMEOUT_SECS, then a 202), ran past the 2-minute limit, or were refused as busy. Errors other than timeouts and busy refusals count toward the API server errors alert. The slow-search tiles in the API workbook show these by endpoint and outcome.'
    severity: 3
    frequency: 'PT15M'
    window: 'PT1H'
    query: slowSearches
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
      severity: r.severity
      enabled: true
      evaluationFrequency: r.frequency
      windowSize: r.window
      scopes: [workspaceId]
      // AppRequests exists only once the first request has been exported.
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

var tests = filter(
  [
    { name: 'site', displayName: 'Site home page', url: siteUrl }
  ],
  t => !empty(t.url)
)

// East US (Virginia), North Central US (Chicago), West US (San Jose).
var testLocations = ['us-va-ash-azr', 'us-il-ch1-azr', 'us-ca-sjc-azr']

// The portal lists a test under the component that its hidden-link tag names.
var linkTag = { 'hidden-link:${appInsightsId}': 'Resource' }

resource webtest 'Microsoft.Insights/webtests@2022-06-15' = [
  for t in tests: {
    name: 'webtest-usnm-${t.name}-${nameSuffix}'
    location: location
    tags: union(tags, linkTag)
    kind: 'standard'
    properties: {
      SyntheticMonitorId: 'webtest-usnm-${t.name}-${nameSuffix}'
      Name: t.displayName
      Description: 'GET ${t.url} expects 200 and a certificate valid for 7 more days.'
      Enabled: true
      Frequency: int(availabilityFrequency)
      Timeout: 30
      Kind: 'standard'
      // A failure counts only after three attempts fail in a row.
      RetryEnabled: true
      Locations: [for l in testLocations: { Id: l }]
      Request: {
        RequestUrl: t.url
        HttpVerb: 'GET'
        ParseDependentRequests: false
      }
      ValidationRules: {
        ExpectedHttpStatusCode: 200
        SSLCheck: true
        SSLCertRemainingLifetimeCheck: 7
      }
    }
  }
]

resource unavailable 'Microsoft.Insights/metricAlerts@2018-03-01' = [
  for (t, i) in tests: {
    name: 'alert-usnm-${t.name}-unavailable-${nameSuffix}'
    location: 'global'
    tags: union(tags, linkTag)
    properties: {
      description: '${t.displayName} (${t.url}) failed from at least 2 of 3 locations. Check the Availability page of the Application Insights resource, then scripts/logs.sh <env> api-errors.'
      severity: 1
      enabled: true
      scopes: [webtest[i].id, appInsightsId]
      evaluationFrequency: 'PT1M'
      // At least one run per location in the window.
      windowSize: { '300': 'PT5M', '600': 'PT10M', '900': 'PT15M' }[availabilityFrequency]
      criteria: {
        'odata.type': 'Microsoft.Azure.Monitor.WebtestLocationAvailabilityCriteria'
        webTestId: webtest[i].id
        componentId: appInsightsId
        failedLocationCount: 2
      }
      autoMitigate: true
      actions: [{ actionGroupId: actionGroupId }]
    }
  }
]
