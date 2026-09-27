// Public DNS zone for the site's domain (08 §8.1). The registrar delegates to
// the zone's name servers (the NAME_SERVERS output). Records:
//
// - apex: an alias to the Static Web App (an apex can't be a CNAME);
// - www: CNAME to the Static Web App;
// - api: CNAME to the API app, plus the `asuid.api` TXT record that
//   Container Apps checks before it binds a custom domain.
//
// Binding the names on the apps themselves (and their managed certificates)
// needs the delegation to be live, so it is a separate step.

param tags object
param zoneName string
param siteId string
param siteHostname string
@description('The API app\'s FQDN; empty skips the api records.')
param apiFqdn string
param apiVerificationId string

resource zone 'Microsoft.Network/dnsZones@2018-05-01' = {
  name: zoneName
  location: 'global'
  tags: tags
}

resource apex 'Microsoft.Network/dnsZones/A@2018-05-01' = {
  parent: zone
  name: '@'
  properties: {
    TTL: 3600
    targetResource: { id: siteId }
  }
}

resource www 'Microsoft.Network/dnsZones/CNAME@2018-05-01' = {
  parent: zone
  name: 'www'
  properties: {
    TTL: 3600
    CNAMERecord: { cname: siteHostname }
  }
}

resource api 'Microsoft.Network/dnsZones/CNAME@2018-05-01' = if (!empty(apiFqdn)) {
  parent: zone
  name: 'api'
  properties: {
    TTL: 3600
    CNAMERecord: { cname: apiFqdn }
  }
}

resource apiVerify 'Microsoft.Network/dnsZones/TXT@2018-05-01' = if (!empty(apiFqdn)) {
  parent: zone
  name: 'asuid.api'
  properties: {
    TTL: 3600
    TXTRecords: [{ value: [apiVerificationId] }]
  }
}

output nameServers array = zone.properties.nameServers
