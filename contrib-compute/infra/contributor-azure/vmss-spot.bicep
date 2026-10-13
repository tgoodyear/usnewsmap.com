// Phase B, VM option (recommended): a Spot virtual machine scale set that runs
// Debian's boinc-client, attached to the project. Deploy setup.bicep first and
// store the BOINC account key in the vault. Each VM fetches the key with its
// managed identity at first boot; no key or Azure credential is in the template.
//
// - Spot, eviction policy Delete, max price -1 (pay up to the on-demand price).
// - Flexible orchestration, several VM sizes, lowest-price allocation.
// - No inbound access at all: no public inbound rule, SSH is not reachable.
// - Pinned OS image (no automatic upgrade). Re-deploy with a newer imageVersion.
// - A scheduled-events watcher suspends BOINC and stops the client when Azure
//   announces an eviction.
// Whether the chosen sizes, regions and the subscription's offer allow Spot is
// not checked here; see docs/azure-contributors.md.

param location string = resourceGroup().location

@description('Key Vault made by setup.bicep.')
param vaultName string

@description('Worker identity made by setup.bicep.')
param workerIdentityName string = 'usnm-worker-id'

@description('BOINC project address from the maintainer: lower-case https:// URL ending with a slash.')
param projectUrl string

@description('BOINC tasks to run at once on each VM: at most the vCPUs, and memory / 2.5 GiB.')
@minValue(1)
@maxValue(64)
param tasksPerVm int = 4

@description('Most disk BOINC may use for project files, in GiB.')
@minValue(1)
param maxDiskGib int = 5

@description('How many VMs.')
@minValue(0)
@maxValue(100)
param instanceCount int = 1

@description('Sizes to try, cheapest available first. Each should have at least 4 vCPU and 10 GB.')
param vmSizes array = [
  'Standard_D4as_v6'
  'Standard_D4as_v5'
]

@description('Debian 12 image version, pinned. List versions with: az vm image list --all --publisher debian --offer debian-12 --sku 12-gen2')
param imageVersion string

@description('Any SSH public key. Linux VMs need one; nothing can connect because there is no inbound access.')
param adminSshPublicKey string

@description('Operating system account name. Nothing logs in as it.')
param adminUsername string = 'usnmadmin'

@description('instancePublicIp: a public address per VM with every inbound connection denied (cheap). natGateway: no public address on the VMs; one NAT gateway for outbound (costs more).')
@allowed([
  'instancePublicIp'
  'natGateway'
])
param outboundMode string = 'instancePublicIp'

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

resource workerIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' existing = {
  name: workerIdentityName
}

var nodeEnv = join([
  'MAX_DISK_GIB=${maxDiskGib}'
  'PROJECT_URL=${projectUrl}'
  'TASKS=${tasksPerVm}'
  'USNM_IDENTITY_CLIENT_ID=${workerIdentity.properties.clientId}'
  'VAULT_NAME=${vaultName}'
  ''
], '\n')

var cloudInit = replace(
  replace(
    replace(
      replace(
        replace(
          loadTextContent('cloud-init.yaml'),
          '@@NODE_ENV@@',
          base64(nodeEnv)
        ),
        '@@CONFIG@@',
        loadFileAsBase64('../../scripts/azure/boinc-config.sh')
      ),
      '@@EVENTS@@',
      loadFileAsBase64('../../scripts/azure/boinc-evict.sh')
    ),
    '@@SETUP@@',
    loadFileAsBase64('../../scripts/azure/vm-setup.sh')
  ),
  '@@EVICT_UNIT@@',
  loadFileAsBase64('../../scripts/azure/usnm-boinc-evict.service')
)

resource nsg 'Microsoft.Network/networkSecurityGroups@2023-11-01' = {
  name: 'usnm-nsg'
  location: location
  properties: {
    securityRules: [
      {
        name: 'deny-all-inbound'
        properties: {
          access: 'Deny'
          description: 'Workers only make outbound connections.'
          destinationAddressPrefix: '*'
          destinationPortRange: '*'
          direction: 'Inbound'
          priority: 4000
          protocol: '*'
          sourceAddressPrefix: '*'
          sourcePortRange: '*'
        }
      }
    ]
  }
}

resource natIp 'Microsoft.Network/publicIPAddresses@2023-11-01' = if (outboundMode == 'natGateway') {
  name: 'usnm-nat-ip'
  location: location
  sku: {
    name: 'Standard'
  }
  properties: {
    publicIPAllocationMethod: 'Static'
  }
}

resource nat 'Microsoft.Network/natGateways@2023-11-01' = if (outboundMode == 'natGateway') {
  name: 'usnm-nat'
  location: location
  sku: {
    name: 'Standard'
  }
  properties: {
    publicIpAddresses: [
      {
        id: natIp.id
      }
    ]
  }
}

// 100.64.0.0/24 is the shared address space (RFC 6598), used only inside this VNet.
resource vnet 'Microsoft.Network/virtualNetworks@2023-11-01' = {
  name: 'usnm-vnet'
  location: location
  properties: {
    addressSpace: {
      addressPrefixes: [
        '100.64.0.0/24'
      ]
    }
    subnets: [
      {
        name: 'workers'
        properties: {
          addressPrefix: '100.64.0.0/24'
          networkSecurityGroup: {
            id: nsg.id
          }
          natGateway: outboundMode == 'natGateway'
            ? {
                id: nat.id
              }
            : null
        }
      }
    ]
  }
}

var publicIpConfig = outboundMode == 'instancePublicIp'
  ? {
      name: 'outbound'
      properties: {
        idleTimeoutInMinutes: 15
      }
      sku: {
        name: 'Standard'
      }
    }
  : null

resource scaleSet 'Microsoft.Compute/virtualMachineScaleSets@2024-07-01' = {
  name: 'usnm-workers'
  location: location
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: {
      '${workerIdentity.id}': {}
    }
  }
  sku: {
    name: 'Mix'
    capacity: instanceCount
  }
  properties: {
    orchestrationMode: 'Flexible'
    platformFaultDomainCount: 1
    singlePlacementGroup: false
    skuProfile: {
      allocationStrategy: 'LowestPrice'
      vmSizes: [for size in vmSizes: { name: size }]
    }
    virtualMachineProfile: {
      priority: 'Spot'
      evictionPolicy: 'Delete'
      billingProfile: {
        maxPrice: -1
      }
      scheduledEventsProfile: {
        terminateNotificationProfile: {
          enable: true
          notBeforeTimeout: 'PT15M'
        }
      }
      osProfile: {
        computerNamePrefix: 'usnm'
        adminUsername: adminUsername
        customData: base64(cloudInit)
        linuxConfiguration: {
          disablePasswordAuthentication: true
          provisionVMAgent: true
          ssh: {
            publicKeys: [
              {
                path: '/home/${adminUsername}/.ssh/authorized_keys'
                keyData: adminSshPublicKey
              }
            ]
          }
          patchSettings: {
            patchMode: 'ImageDefault'
          }
        }
      }
      storageProfile: {
        imageReference: {
          publisher: 'debian'
          offer: 'debian-12'
          sku: '12-gen2'
          version: imageVersion
        }
        osDisk: {
          createOption: 'FromImage'
          caching: 'ReadOnly'
          managedDisk: {
            storageAccountType: 'StandardSSD_LRS'
          }
          diskSizeGB: 64
        }
      }
      networkProfile: {
        networkApiVersion: '2020-11-01'
        networkInterfaceConfigurations: [
          {
            name: 'nic'
            properties: {
              primary: true
              enableAcceleratedNetworking: true
              networkSecurityGroup: {
                id: nsg.id
              }
              ipConfigurations: [
                {
                  name: 'ipconfig'
                  properties: {
                    primary: true
                    subnet: {
                      id: vnet.properties.subnets[0].id
                    }
                    publicIPAddressConfiguration: publicIpConfig
                  }
                }
              ]
            }
          }
        ]
      }
    }
  }
}
