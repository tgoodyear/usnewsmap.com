// Phase B, container option: N Azure Container Instances groups that run the
// official BOINC client image (boinc/client), attached to the project. Deploy
// setup.bicep first and store the BOINC account key in the vault. An init
// container fetches the key with the managed identity and writes the account
// file and preferences into a volume that the client container then uses. There
// is no custom image.
//
// Spot is the default priority. ACI Spot is in preview, is limited to 4 vCPU
// and 16 GB per group, and is offered in a few regions only; see
// docs/azure-contributors.md for the sources. The BOINC client runs native
// apps only here: ACI gives no container runtime, so Docker-wrapper apps
// cannot run.

param location string = resourceGroup().location

@description('Key Vault made by setup.bicep.')
param vaultName string

@description('Worker identity made by setup.bicep.')
param workerIdentityName string = 'usnm-worker-id'

@description('BOINC project address from the maintainer: lower-case https:// URL ending with a slash.')
param projectUrl string

@description('Official BOINC client image. Pin by digest (name@sha256:...) once the maintainer has checked one. The image is ubuntu plus a PPA build of boinc-client, last pushed 2025-05-04 when read on 2026-10-13.')
param boincImage string = 'docker.io/boinc/client:latest'

@description('Image for the init container: needs apt, bash and ca-certificates. Pin by digest.')
param toolboxImage string = 'docker.io/library/debian:12-slim'

@description('Most disk BOINC may use for project files, in GiB.')
@minValue(1)
param maxDiskGib int = 5

@description('How many container groups.')
@minValue(0)
@maxValue(100)
param groupCount int = 1

@description('vCPU per group. Spot allows at most 4.')
@minValue(1)
@maxValue(4)
param vcpu int = 4

@description('Memory in GB per group. The worker needs 2.5 GB per concurrent page; Spot allows at most 16.')
@minValue(1)
@maxValue(16)
param memoryGb int = 10

@description('Spot costs less and can be evicted; Regular is not evicted.')
@allowed([
  'Spot'
  'Regular'
])
param priority string = 'Spot'

@description('What ACI does when a container exits. BOINC should always run.')
@allowed([
  'Always'
  'OnFailure'
  'Never'
])
param restartPolicy string = 'Always'

@description('Monthly budget alert amount. 0 skips the budget.')
param budgetAmount int = 0

param budgetContactEmails array = []

module budget 'modules/budget.bicep' = if (budgetAmount > 0) {
  name: 'budget'
  params: {
    amount: budgetAmount
    contactEmails: budgetContactEmails
  }
}

var configScript = loadFileAsBase64('../../scripts/azure/boinc-config.sh')
// BOINC tasks at once: at most the vCPUs, and memory / 2.5 GiB.
var tasks = min(vcpu, memoryGb * 1024 / 2560)

resource workerIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' existing = {
  name: workerIdentityName
}

resource workers 'Microsoft.ContainerInstance/containerGroups@2023-05-01' = [
  for i in range(0, groupCount): {
    name: 'usnm-worker-${i}'
    location: location
    identity: {
      type: 'UserAssigned'
      userAssignedIdentities: {
        '${workerIdentity.id}': {}
      }
    }
    properties: {
      osType: 'Linux'
      priority: priority
      restartPolicy: restartPolicy
      initContainers: [
        {
          name: 'fetch-config'
          properties: {
            image: toolboxImage
            command: [
              '/bin/bash'
              '-c'
              'apt-get update -qq && apt-get install -y -qq --no-install-recommends curl jq ca-certificates > /dev/null && bash /scripts/boinc-config.sh --vault-name "$VAULT_NAME" --project-url "$PROJECT_URL" --out-dir /var/lib/boinc --tasks "$TASKS" --max-disk-gib "$MAX_DISK_GIB" --identity-client-id "$USNM_IDENTITY_CLIENT_ID"'
            ]
            environmentVariables: [
              { name: 'MAX_DISK_GIB', value: string(maxDiskGib) }
              { name: 'PROJECT_URL', value: projectUrl }
              { name: 'TASKS', value: string(tasks) }
              { name: 'USNM_IDENTITY_CLIENT_ID', value: workerIdentity.properties.clientId }
              { name: 'VAULT_NAME', value: vaultName }
            ]
            volumeMounts: [
              {
                name: 'scripts'
                mountPath: '/scripts'
                readOnly: true
              }
              {
                name: 'data'
                mountPath: '/var/lib/boinc'
              }
            ]
          }
        }
      ]
      containers: [
        {
          name: 'boinc'
          properties: {
            image: boincImage
            resources: {
              requests: {
                cpu: vcpu
                memoryInGB: memoryGb
              }
            }
            volumeMounts: [
              {
                name: 'data'
                mountPath: '/var/lib/boinc'
              }
            ]
          }
        }
      ]
      volumes: [
        {
          name: 'scripts'
          secret: {
            'boinc-config.sh': configScript
          }
        }
        {
          name: 'data'
          emptyDir: {}
        }
      ]
    }
  }
]
