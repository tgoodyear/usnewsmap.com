// The server-side half of the usnm-boinc bridge: a manual-trigger Container Apps Job that
// runs `usnm-boinc submit` (and the reconcile step) in the same image as the server, with the
// same project share mounted. stock `create_work` and `bin/dir_hier_path` need the
// project tree, config.xml and the database, so they cannot run anywhere else. The job
// holds no ocr-out or curated credential: export and collect stay on the maintainer's side.
// Nothing starts it on a schedule.
param jobName string
param location string
param environmentId string
param identityId string
param identityClientId string
param image string
@description('Registry login server, taken from the image reference. The app pulls with its identity (AcrPull); the registry has no admin user or anonymous pull.')
param registryServer string
param keyVaultName string
param mysqlHost string
param databaseName string
param masterHost string
param projectStorageName string
param workloadProfileName string
@description('The container command. The default runs the bridge; the project-init job passes the entrypoint\'s --init-project.')
param command array = [ '/usr/local/bin/bridge-job' ]
@description('Seconds before a run is stopped.')
param timeoutSeconds int = 1800
@description('Settings added to the job\'s own, as { name, value } (the gold bundle job sets its step this way).')
param extraEnv array = []
@description('CPU cores and memory of a run.')
param cpu string = '0.5'
param memory string = '1Gi'
@description('The project share\'s mount options. mfsymlinks stores BOINC\'s symlinks as small files on SMB.')
param mountOptions string = 'mfsymlinks,uid=1000,gid=1000,dir_mode=0775,file_mode=0775,cache=strict'
@description('Whether /project is mounted. The share copy job reaches both shares over REST only, so it mounts neither: the mount is read-write with the account key.')
param mountProject bool = true

resource job 'Microsoft.App/jobs@2024-03-01' = {
  name: jobName
  location: location
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: { '${identityId}': {} }
  }
  properties: {
    environmentId: environmentId
    workloadProfileName: workloadProfileName
    configuration: {
      registries: [ { server: registryServer, identity: identityId } ]
      triggerType: 'Manual'
      replicaTimeout: timeoutSeconds
      replicaRetryLimit: 0 // submit must never be retried by the platform (usnm-boinc journals and refuses replays)
      manualTriggerConfig: { parallelism: 1, replicaCompletionCount: 1 }
    }
    template: {
      containers: [
        {
          name: 'bridge'
          image: image
          command: command
          resources: { cpu: json(cpu), memory: memory }
          env: concat([
            { name: 'AZURE_CLIENT_ID', value: identityClientId }
            { name: 'KEY_VAULT_NAME', value: keyVaultName }
            { name: 'DB_HOST', value: mysqlHost }
            { name: 'DB_NAME', value: databaseName }
            { name: 'MASTER_HOST', value: masterHost }
            { name: 'PROJECT_DIR', value: '/project' }
            { name: 'HANDOFF_DIR', value: '/project/handoff' }
          ], extraEnv)
          volumeMounts: mountProject ? [ { volumeName: 'project', mountPath: '/project' } ] : []
        }
      ]
      // SMB has no symlinks or POSIX owners: mfsymlinks stores BOINC's symlinks (bin/stop ->
      // bin/start), and the share appears owned by boincadm (uid 1000).
      volumes: mountProject ? [ { name: 'project', storageType: 'AzureFile', storageName: projectStorageName, mountOptions: mountOptions } ] : []
    }
  }
}

output name string = job.name
