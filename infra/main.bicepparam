// Parameters for infra/main.bicep, from the environment's settings
// (.azure/<env>/.env). scripts/bootstrap.sh exports the settings that have
// a value before it deploys the stack; the rest take the defaults here.
using './main.bicep'

param environmentName = readEnvironmentVariable('AZURE_ENV_NAME')
param location = readEnvironmentVariable('AZURE_LOCATION', 'eastus2')
param apiImage = readEnvironmentVariable('USNM_API_IMAGE', '')
param searchBackend = readEnvironmentVariable('USNM_SEARCH_BACKEND', 'fixtures')
param ingestJobs = bool(readEnvironmentVariable('USNM_INGEST_JOBS', 'false'))
param useAcr = bool(readEnvironmentVariable('USNM_USE_ACR', 'false'))
param imageTag = readEnvironmentVariable('USNM_IMAGE_TAG', 'main')
param ingestCron = readEnvironmentVariable('USNM_INGEST_CRON', '')
param backfillWorkers = int(readEnvironmentVariable('USNM_BACKFILL_WORKERS', '8'))
param cosmosFreeTier = bool(readEnvironmentVariable('USNM_COSMOS_FREE_TIER', 'true'))
param alertEmails = readEnvironmentVariable('USNM_ALERT_EMAILS', '')
param budgetStartDate = readEnvironmentVariable('USNM_BUDGET_START', '')
param dnsZoneName = readEnvironmentVariable('USNM_DNS_ZONE', '')
param apiCertificateId = readEnvironmentVariable('USNM_API_CERT_ID', '')
param siteCertificateId = readEnvironmentVariable('USNM_SITE_CERT_ID', '')
param wwwCertificateId = readEnvironmentVariable('USNM_WWW_CERT_ID', '')
param githubRepo = readEnvironmentVariable('USNM_GITHUB_REPO', 'tgoodyear/usnewsmap.com')
param githubRepoIds = readEnvironmentVariable('USNM_GITHUB_REPO_IDS', 'tgoodyear@116683/usnewsmap.com@1389862972')
