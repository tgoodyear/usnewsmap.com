// The public API (08 §8.1). This slice runs the API on its baked synthetic
// fixtures (memory backend) while exercising the production paths: managed
// identity, the Blob private endpoint (persistent response cache), ingress,
// probes and scaling. The Quickwit searcher sidecar joins once spike S-2
// settles how Quickwit authenticates to Blob without account keys.

param location string
param tags object
param name string
param environmentId string
param image string
param identityId string
param identityClientId string
param storageBlobEndpoint string
param allowedOrigins array
param minReplicas int
param maxReplicas int = 2
@description('Proxies appending to X-Forwarded-For: 1 = Container Apps ingress only.')
param trustedProxyHops int = 1

resource app 'Microsoft.App/containerApps@2024-03-01' = {
  name: name
  location: location
  tags: tags
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: { '${identityId}': {} }
  }
  properties: {
    environmentId: environmentId
    workloadProfileName: 'Consumption'
    configuration: {
      activeRevisionsMode: 'Single'
      ingress: {
        external: true
        targetPort: 8080
        transport: 'http'
        allowInsecure: false
      }
    }
    template: {
      containers: [
        {
          name: 'api'
          image: image
          resources: { cpu: json('0.25'), memory: '0.5Gi' }
          env: [
            { name: 'USNM_BACKEND', value: 'memory' }
            { name: 'USNM_RESPONSE_CACHE_URL', value: '${storageBlobEndpoint}cache' }
            { name: 'USNM_ALLOWED_ORIGINS', value: join(allowedOrigins, ',') }
            { name: 'USNM_TRUSTED_PROXY_HOPS', value: string(trustedProxyHops) }
            // Selects the user-assigned identity at the managed identity endpoint.
            { name: 'AZURE_CLIENT_ID', value: identityClientId }
          ]
          probes: [
            {
              type: 'Liveness'
              httpGet: { path: '/healthz', port: 8080 }
              periodSeconds: 30
            }
            {
              type: 'Readiness'
              httpGet: { path: '/readyz', port: 8080 }
              periodSeconds: 10
            }
          ]
        }
      ]
      scale: {
        minReplicas: minReplicas
        maxReplicas: maxReplicas
        rules: [
          {
            name: 'http'
            http: { metadata: { concurrentRequests: '50' } }
          }
        ]
      }
    }
  }
}

output fqdn string = app.properties.configuration.ingress.fqdn
