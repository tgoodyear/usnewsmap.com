// Parameters for infra/policy-exemptions.bicep, from the same settings as
// infra/main.bicepparam (scripts/lib/env.sh exports them for both).
using './policy-exemptions.bicep'

import { checkedEnvName } from 'naming.bicep'

param environmentName = checkedEnvName(readEnvironmentVariable('AZURE_ENV_NAME'))
param ocrNodes = bool(readEnvironmentVariable('USNM_OCR_NODES', 'false'))
param ocrNodeIps = readEnvironmentVariable('USNM_OCR_NODE_IPS', '')
param serialSetOnPrem = bool(readEnvironmentVariable('USNM_SERIALSET_ONPREM', 'false'))
param jaOcrOnPrem = bool(readEnvironmentVariable('USNM_JA_OCR_ONPREM', 'false'))
param onPremRelease = bool(readEnvironmentVariable('USNM_ONPREM_RELEASE', 'false'))
param indexStorage = bool(readEnvironmentVariable('USNM_INDEX_STORAGE', 'false'))
param onPremServing = bool(readEnvironmentVariable('USNM_ONPREM_SERVING', 'false'))
param cosmosOnPremAccess = bool(readEnvironmentVariable('USNM_COSMOS_ONPREM_ACCESS', 'false'))
param ocrSyncOnPrem = bool(readEnvironmentVariable('USNM_OCR_SYNC_ONPREM', 'false'))
