# ADR-0030: Remove private networking from prod: no private endpoints, no VNet

- **Status:** Accepted by the maintainer, 6 November 2026 ([#308](https://github.com/tgoodyear/usnewsmap.com/issues/308)).
- **Date:** 2026-11
- **Supersedes:** [ADR-0008](0008-private-networking.md) for prod. ADR-0008 still describes the default of every other environment, because `USNM_DATA_PRIVATE_ENDPOINTS` and `USNM_CAE_VNET` default to the private design.
- **Relates to:** [ADR-0009](0009-entra-identity-only.md), [ADR-0027](0027-onprem-serving-behind-cloudflare.md), [ADR-0029](0029-onprem-primary-azure-failover.md); #308, #1425, #1600, #1602, #1611, #1614, #1615

## Context

[ADR-0008](0008-private-networking.md) put the data lake, the index account and Cosmos behind private endpoints, with the API and searcher in a VNet-integrated Container Apps environment.

Since 31 October 2026 public traffic is served on-prem, and the Azure API and searcher sit at zero replicas as the cold spare ([ADR-0029](0029-onprem-primary-azure-failover.md)). The private network carried no traffic until a failover, and it still cost money (#308, "Running cost"):

- Three private endpoints and two privatelink zones cost $0.72 a day, about $22.90 a month (#1425, #1600).
- The Container Apps environment's load balancer and public IP, `capp-svc-lb` and `capp-svc-lb-ip`, cost about $21.90 a month. They exist only because the environment is VNet-integrated (#1611).

The maintainer authorized both removals on 6 November 2026 (#1600, #1611). The record does not say why they chose to remove the network layer rather than keep it and cut cost elsewhere, beyond the cost figures above.

## Decision

Three steps, each a pull request followed by `scripts/azure/provision.sh --env prod --api-tag live`. Every apply followed a what-if whose delete list was checked first, and none listed a data store.

1. **Remove the private endpoints (#1600, #1602).** `USNM_DATA_SUBNET_ACCESS` first added service endpoints on snet-cae and a firewall rule on each account while the endpoints stayed (stage 1, provisioned on 6 November, 16:23 to 16:48 UTC). `USNM_DATA_PRIVATE_ENDPOINTS=false` then deleted `pe-usnm-blob`, `pe-usnm-cosmos`, `usnm-qw-blob-pep-prod` and their NICs, plus `privatelink.blob.core.windows.net` and `privatelink.documents.azure.com` with their links (stage 2, 17:44 to 18:06 UTC).
2. **Rebuild the spare without a VNet (#1611, #1614).** `USNM_SPARE_ENVIRONMENT` added `usnm-core-spare-cae-prod` with no VNet, plus `usnm-api-spare-ca-prod` and `usnm-qw-spare-ca-prod`. `USNM_DATA_ALL_NETWORKS` opened the data lake, the index account and Cosmos to every network. #1611 also retired the BOINC hand-off job `usnm-boinc-handoff-caj-prod`, which last ran on 25 October, because claim, collect and export run on-prem (#967, #1072). #1614 pointed the fallback Worker at the new API's default hostname.
3. **Remove the old environment (#1615).** `USNM_CAE_VNET=false` removed `cae-usnm-prod`, `ca-usnm-prod`, `usnm-qw-search-ca-prod`, their role assignments and `vnet-usnm-prod`. Azure removed the `ME_` group with the load balancer and public IP. `origin-tls` keeps the origin certificate in Key Vault and no longer uploads it, because no Azure app carries the custom domains.

**End state in prod**

| Account              | Network access                                        |
| -------------------- | ----------------------------------------------------- |
| Data lake            | Public, every network (`defaultAction: Allow`)        |
| Index account        | Public, every network                                 |
| Cosmos               | Public, every network (no IP rules, VNet filter off)  |
| OCR exchange account | Public, `defaultAction: Deny`, on-prem addresses only |
| Scan archive         | Public, `defaultAction: Deny`, on-prem addresses only |

The exchange account and the scan archive keep their firewalls because nothing in Azure reads them once the hand-off job is gone (#1611). The first three are listed by resource id in the public-network policy.

**Identity is unchanged.** Shared keys, Cosmos local auth, SAS and anonymous access stay off, and the policies that enforce that are unchanged ([ADR-0009](0009-entra-identity-only.md)). A request needs an Entra token for an identity with a data-plane role. The roles did not change.

**Other environments are unchanged.** `USNM_CAE_VNET` defaults to `true`. `USNM_DATA_ALL_NETWORKS` defaults to `false`, and a bicep guard requires `USNM_DATA_PRIVATE_ENDPOINTS=false` with it. `USNM_CAE_VNET=false` needs `USNM_DATA_ALL_NETWORKS` and no `USNM_ARCHIVE_ACCOUNT`, because the archive account's endpoint needs the VNet (#1615).

## Alternatives

From the amendments to ADR-0008 and #1425:

| Option                                            | Why not                                                                                                                                                                     |
| ------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Keep the VNet                                     | $21.90 a month for a path that is idle until a failover                                                                                                                     |
| Remove only the lake's and Cosmos's endpoints     | Considered in #1425: it saved $15.10 a month and kept the index account closed. The maintainer authorized the full removal; the record does not say why this was not chosen |
| IP rules for the environment's outbound addresses | A VNet-less environment's outbound addresses are shared with other tenants and can change, so a rule would admit others and could drop the spare during a failover          |
| A NAT gateway for a fixed outbound address        | Needs the VNet, and costs more than the load balancer it would replace                                                                                                      |

## Consequences

- **Saves about $44.80 a month** in the two removals ($22.90 plus $21.90). #308 puts the recurring cost at about $199 a month before and about $105 after this and two other cuts (#1608, #1609), still above the $80 ceiling of [ADR-0005](0005-sustainability-constraints.md). Those are estimates until settled cost data arrives.
- **The network layer is gone.** Before, a stolen token was useless outside snet-cae or the on-prem address. Now it works from any network until it expires, about an hour for an Entra access token. The accounts' public endpoints answer everyone and refuse anyone without a role. Storage and Cosmos resource logs still record every authenticated caller.
- **The guarded public window in ADR-0008 no longer applies in prod.** Its `network-guard` job and window items belong to the private design.
- **Checked cold, at zero replicas, on the new API's default hostname** (#308): `/readyz` answered 200 after 65 s following stage 1 of the VNet-less spare and after 57 s following its stage 2, inside the Worker's 180 s budget. `/v1/meta`, `/v1/status` and uncached `/v1/aggregate` and `/v1/hits` answered correctly. The new searcher logged eight 403s at its first start while its role propagated, and none after. The full Worker drill was still pending when #308 recorded this.
- **Rollback.** Each setting can be turned back and provisioned. `USNM_DATA_ALL_NETWORKS` off restores the IP and subnet rules, which stay in the template. Whether a rollback of `USNM_CAE_VNET` has been tried is not recorded.
- The runbook is [search-serving.md](../../operations/search-serving.md#the-spares-environment-without-a-vnet).
