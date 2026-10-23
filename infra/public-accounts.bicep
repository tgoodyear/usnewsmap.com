// The resources the public-network policy (usnm-audit-data-public-network,
// guardrails.bicep) lets have public network access in one environment:
// its publicAccountIds. main.bicep (the stack) and policy-exemptions.bicep
// (the step provision runs before the stack, #336) both take the list from
// here, so the two deployments write the same assignment.
//
// - with ocrNodes, the OCR exchange account, the scan archive account and
//   the OCR nodes' Key Vault, whose firewalls allow only the node IPs (ADR-0015);
// - with lakeOpen, the data lake, whose firewall allows only the node IPs
//   while the on-prem Serial Set runner writes to it (ADR-0008, amendment
//   of October 2026);
// - with indexOpen, the separate index account, whose firewall allows only
//   the node IPs while an on-prem release copies a staged version into it
//   (onPremRelease, ADR-0021 amendment of October 2026). Otherwise it stays
//   private-only, as before;
// - with cosmosOpen, the Cosmos account, whose firewall allows only the
//   node IPs while the on-prem serving container reads pipeline state
//   (onPremServing, #955) or the on-prem BOINC host runs claim and collect
//   (cosmosOnPremAccess, #967). The private endpoint stays for the Azure
//   apps. Otherwise it stays private-only.

import { Names } from 'naming.bicep'

@export()
func publicAccountIds(
  subscriptionId string,
  n Names,
  ocrNodes bool,
  lakeOpen bool,
  indexOpen bool,
  cosmosOpen bool
) array =>
  concat(
    ocrNodes
      ? [
          resourceId(subscriptionId, n.rg, 'Microsoft.Storage/storageAccounts', n.ocrExchange)
          resourceId(subscriptionId, n.rg, 'Microsoft.Storage/storageAccounts', n.scansAccount)
          resourceId(subscriptionId, n.rg, 'Microsoft.KeyVault/vaults', n.ocrVault)
        ]
      : [],
    lakeOpen ? [resourceId(subscriptionId, n.rg, 'Microsoft.Storage/storageAccounts', n.dataLake)] : [],
    indexOpen ? [resourceId(subscriptionId, n.rg, 'Microsoft.Storage/storageAccounts', n.indexAccount)] : [],
    cosmosOpen ? [resourceId(subscriptionId, n.rg, 'Microsoft.DocumentDB/databaseAccounts', n.cosmos)] : []
  )

// The data lake opens to the node IPs only for an on-prem host that uses it
// (the Serial Set's and the Japanese OCR's runners, the release, the serving
// container), and only when there are IPs to allow.
@export()
func lakeOpen(onPremLakeRunner bool, ocrNodeIps string) bool =>
  onPremLakeRunner && !empty(nodeIpList(ocrNodeIps))

// The index account opens to the node IPs only while an on-prem release
// publishes (onPremRelease), and only when there is an index account and
// IPs to allow.
@export()
func indexOpen(onPremRelease bool, indexStorage bool, ocrNodeIps string) bool =>
  onPremRelease && indexStorage && !empty(nodeIpList(ocrNodeIps))

// The Cosmos account opens to the node IPs only when a setting asks for it
// (onPremServing or cosmosOnPremAccess), and only when there are IPs to
// allow: with public access on and no IP rules, Cosmos would accept every network.
@export()
func cosmosOpen(open bool, ocrNodeIps string) bool =>
  open && !empty(nodeIpList(ocrNodeIps))

// USNM_OCR_NODE_IPS as a list: comma separated, blanks dropped.
@export()
func nodeIpList(ocrNodeIps string) array => filter(map(split(ocrNodeIps, ','), ip => trim(ip)), ip => !empty(ip))
