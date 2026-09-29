// Public DNS zone for the site's domain (08 §8.1). The registrar delegates to
// the zone's name servers (the NAME_SERVERS output). Records:
//
// - apex: an A record to the Container Apps environment's static IP (an apex
//   can't be a CNAME), and CAA records limiting certificate issuance (08 §8.8);
// - no-mail records: SPF `-all`, DMARC reject, empty DKIM keys;
// - www and api: CNAMEs to the app, which serves both the site and the API;
// - `asuid`, `asuid.www`, `asuid.api`: the TXT records Container Apps checks
//   before it binds each custom domain.
//
// Binding the names on the app (and their managed certificates) needs the
// delegation to be live, so it is a separate step (scripts/bootstrap.sh).

param tags object
param zoneName string
@description('The app\'s FQDN; empty (no app yet) skips the www and api records.')
param appFqdn string
@description('The Container Apps environment\'s static IP, for the apex.')
param staticIp string
@description('The environment\'s custom domain verification ID, for the asuid records.')
param verificationId string
@description('CAs allowed to issue for the domain: Container Apps managed certificates come from DigiCert.')
param caaIssuers array = ['digicert.com']
@description('More TXT values at the apex, such as site-verification tokens.')
param apexTxtValues array = []

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
// carry over the records the domain had before moving to Azure DNS. The apex
// TXT set also holds any site-verification tokens (apexTxtValues).
resource spf 'Microsoft.Network/dnsZones/TXT@2018-05-01' = {
  parent: zone
  name: '@'
  properties: {
    TTL: 3600
    TXTRecords: concat([{ value: ['v=spf1 -all'] }], map(apexTxtValues, v => { value: [v] }))
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
    ARecords: [{ ipv4Address: staticIp }]
  }
}

var cnames = empty(appFqdn) ? [] : ['www', 'api']

resource hosts 'Microsoft.Network/dnsZones/CNAME@2018-05-01' = [
  for name in cnames: {
    parent: zone
    name: name
    properties: {
      TTL: 3600
      CNAMERecord: { cname: appFqdn }
    }
  }
]

resource verify 'Microsoft.Network/dnsZones/TXT@2018-05-01' = [
  for name in ['asuid', 'asuid.www', 'asuid.api']: {
    parent: zone
    name: name
    properties: {
      TTL: 3600
      TXTRecords: [{ value: [verificationId] }]
    }
  }
]

output nameServers array = zone.properties.nameServers
