// Archival storage (docs/operations.md, "Archival storage"): one storage
// account that keeps everything we download from outside Azure, so
// re-curation, benchmarks and other environments never download it again:
// LoC's batch archives and listings (`raw`) and packaged sample sets
// (`sets`) now, other sources later. It sits outside every environment's stack, in
// its own stack `usnm-archive` and group `rg-usnm-archive`, deployed once with
// scripts/archive-store.sh, so no environment's provision or teardown can
// delete it. Environments reach it through a private endpoint of their own
// (`USNM_ARCHIVE_ACCOUNT`, infra/modules/archive-access.bicep).

targetScope = 'subscription'

@description('Region; the environments\' (East US 2).')
param location string = 'eastus2'

@description('The account\'s name: stable per subscription.')
param name string = 'stusnmarchive${take(uniqueString(subscription().id, 'usnm-archive'), 6)}'

var tags = {
  project: 'usnewsmap'
  environment: 'archive'
}

resource rg 'Microsoft.Resources/resourceGroups@2024-03-01' = {
  name: 'rg-usnm-archive'
  location: location
  tags: tags
}

module store 'store.bicep' = {
  scope: rg
  name: 'archive-store'
  params: {
    location: location
    tags: tags
    name: name
  }
}

output ARCHIVE_ACCOUNT_ID string = store.outputs.id
output ARCHIVE_ACCOUNT string = store.outputs.name
output ARCHIVE_RESOURCE_GROUP string = rg.name
