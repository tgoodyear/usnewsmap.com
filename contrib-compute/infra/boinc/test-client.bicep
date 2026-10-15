// Ephemeral BOINC volunteer test client VM (#484), deployed into its own resource group
// usnm-boinc-test-rg-<env> by scripts/boinc-test-client.sh and removed by deleting that group.
//
// Outbound-only: the NSG denies all inbound traffic. Cloud-init installs Docker, the NVIDIA
// driver and container toolkit when a GPU is present, and boinc-client, then attaches to the
// project with the authenticator passed by Key Vault secret reference.
targetScope = 'resourceGroup'

import { checkedEnvName, usnmName } from '../../../infra/naming.bicep'

@description('Environment: prod or dev (ADR-0017).')
param env string

param location string = resourceGroup().location

@description('VM size: GPU (Standard_NC4as_T4_v3) or CPU (for example Standard_D4s_v5).')
param vmSize string = 'Standard_NC4as_T4_v3'

@description('Run as an Azure Spot VM with Delete eviction policy.')
param spot bool = true

@description('BOINC project master URL, for example https://boinc.usnewsmap.com/usnmocr/.')
param projectUrl string

@secure()
@description('BOINC account authenticator for the test volunteer account, passed via Key Vault reference.')
param projectAuthenticator string

@description('Admin username for the Linux OS profile.')
param adminUsername string = 'azureuser'

@secure()
@description('Random admin password required by the Linux OS profile; inbound access is blocked by the NSG.')
param adminPasswordOrKey string = 'Aa1-${newGuid()}!'

var envName = checkedEnvName(env)

var names = {
  nsg: usnmName('boinc-test', 'nsg', envName)
  vnet: usnmName('boinc-test', 'vnet', envName)
  pip: usnmName('boinc-test', 'pip', envName)
  nic: usnmName('boinc-test', 'nic', envName)
  vm: usnmName('boinc-test', 'vm', envName)
}

var cloudInitTemplate = '''#!/usr/bin/env bash
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q ca-certificates curl gnupg pciutils docker.io boinc-client ubuntu-drivers-common
usermod -aG docker boinc
if lspci | grep -qi nvidia; then
  ubuntu-drivers install --gpgpu || apt-get install -y -q nvidia-headless-550-server nvidia-utils-550-server
  curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey \
    | gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
  curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
    | sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' \
    > /etc/apt/sources.list.d/nvidia-container-toolkit.list
  apt-get update -q
  apt-get install -y -q nvidia-container-toolkit
  nvidia-ctk runtime configure --runtime=docker
fi
systemctl enable --now docker
systemctl restart docker
systemctl enable --now boinc-client
systemctl restart boinc-client
cd /var/lib/boinc-client
for _ in $(seq 1 30); do
  boinccmd --get_state >/dev/null 2>&1 && break
  sleep 2
done
boinccmd --project_attach '__PROJECT_URL__' '__PROJECT_AUTHENTICATOR__'
'''

var cloudInit = replace(
  replace(cloudInitTemplate, '__PROJECT_URL__', projectUrl),
  '__PROJECT_AUTHENTICATOR__',
  projectAuthenticator
)

resource nsg 'Microsoft.Network/networkSecurityGroups@2024-01-01' = {
  name: names.nsg
  location: location
  properties: {
    securityRules: [
      {
        name: 'deny-all-inbound'
        properties: {
          priority: 100
          direction: 'Inbound'
          access: 'Deny'
          protocol: '*'
          sourcePortRange: '*'
          destinationPortRange: '*'
          sourceAddressPrefix: '*'
          destinationAddressPrefix: '*'
        }
      }
    ]
  }
}

// Uses RFC 6598 shared address space so no RFC 1918 literal appears in the template.
resource vnet 'Microsoft.Network/virtualNetworks@2024-01-01' = {
  name: names.vnet
  location: location
  properties: {
    addressSpace: {
      addressPrefixes: [ '100.64.0.0/24' ]
    }
    subnets: [
      {
        name: 'default'
        properties: {
          addressPrefix: '100.64.0.0/24'
          networkSecurityGroup: {
            id: nsg.id
          }
        }
      }
    ]
  }
}

resource pip 'Microsoft.Network/publicIPAddresses@2024-01-01' = {
  name: names.pip
  location: location
  sku: {
    name: 'Standard'
  }
  properties: {
    publicIPAllocationMethod: 'Static'
  }
}

resource nic 'Microsoft.Network/networkInterfaces@2024-01-01' = {
  name: names.nic
  location: location
  properties: {
    networkSecurityGroup: {
      id: nsg.id
    }
    ipConfigurations: [
      {
        name: 'ipconfig1'
        properties: {
          privateIPAllocationMethod: 'Dynamic'
          subnet: {
            id: '${vnet.id}/subnets/default'
          }
          publicIPAddress: {
            id: pip.id
          }
        }
      }
    ]
  }
}

resource vm 'Microsoft.Compute/virtualMachines@2024-03-01' = {
  name: names.vm
  location: location
  properties: {
    hardwareProfile: {
      vmSize: vmSize
    }
    priority: spot ? 'Spot' : 'Regular'
    evictionPolicy: spot ? 'Delete' : null
    billingProfile: spot ? {
      maxPrice: -1
    } : null
    storageProfile: {
      imageReference: {
        publisher: 'Canonical'
        offer: 'ubuntu-24_04-lts'
        sku: 'server'
        version: 'latest'
      }
      osDisk: {
        createOption: 'FromImage'
        diskSizeGB: 64
        deleteOption: 'Delete'
        managedDisk: {
          storageAccountType: 'StandardSSD_LRS'
        }
      }
    }
    osProfile: {
      computerName: names.vm
      adminUsername: adminUsername
      adminPassword: adminPasswordOrKey
      customData: base64(cloudInit)
    }
    networkProfile: {
      networkInterfaces: [
        {
          id: nic.id
          properties: {
            deleteOption: 'Delete'
          }
        }
      ]
    }
    diagnosticsProfile: {
      bootDiagnostics: {
        enabled: true
      }
    }
  }
}

output vmName string = vm.name
