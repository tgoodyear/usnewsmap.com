// Public DNS zone for the site's domain (08 §8.1). The registrar delegates to
// the zone's name servers (the NAME_SERVERS output). Records:
//
// - apex: an alias to the Static Web App (an apex can't be a CNAME), and
//   CAA records limiting certificate issuance (08 §8.9);
// - no-mail records: SPF `-all`, DMARC reject, empty DKIM keys;
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
@description('CAs allowed to issue for the domain: the Static Web Apps and Container Apps managed certificates both come from DigiCert.')
param caaIssuers array = ['digicert.com']

resource zone 'Microsoft.Network/dnsZones@2018-05-01' = {
  name: zoneName
  location: 'global'
  tags: tags
}

// Only the managed-certificate CA may issue, and nobody may issue wildcards.
resource caa 'Microsoft.Network/dnsZones/CAA@2018-05-01' = {
  parent: zone
  name: '@'
  properties: {
    TTL: 3600
    caaRecords: concat(
      map(caaIssuers, ca => { flags: 0, tag: 'issue', value: ca }),
      [{ flags: 0, tag: 'issuewild', value: ';' }]
    )
  }
}

// The domain sends no mail: SPF allows no senders, DMARC rejects anything
// that claims it, and every DKIM selector has an empty (revoked) key. These
// carry over the records the domain had before moving to Azure DNS.
resource spf 'Microsoft.Network/dnsZones/TXT@2018-05-01' = {
  parent: zone
  name: '@'
  properties: {
    TTL: 3600
    TXTRecords: [{ value: ['v=spf1 -all'] }]
  }
}

resource dmarc 'Microsoft.Network/dnsZones/TXT@2018-05-01' = {
  parent: zone
  name: '_dmarc'
  properties: {
    TTL: 3600
    TXTRecords: [{ value: ['v=DMARC1; p=reject; sp=reject; adkim=s; aspf=s;'] }]
  }
}

resource dkim 'Microsoft.Network/dnsZones/TXT@2018-05-01' = {
  parent: zone
  name: '*._domainkey'
  properties: {
    TTL: 3600
    TXTRecords: [{ value: ['v=DKIM1; p='] }]
  }
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
