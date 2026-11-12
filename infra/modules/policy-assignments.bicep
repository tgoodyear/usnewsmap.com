// Assign the project guardrails to one resource group.

param definitionIds array
@description('Assignment parameters by definition name, e.g. { name: { param: { value: x } } }.')
param definitionParameters object = {}

resource assignments 'Microsoft.Authorization/policyAssignments@2024-04-01' = [
  for id in definitionIds: {
    name: take(last(split(id, '/')), 64)
    properties: {
      policyDefinitionId: id
      // The service's default, written out so a what-if doesn't show it as removed.
      definitionVersion: '1.*.*'
      enforcementMode: 'Default'
      parameters: definitionParameters[?last(split(id, '/'))] ?? {}
    }
  }
]
