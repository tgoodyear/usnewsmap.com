// Assign the project guardrails to one resource group.

param definitionIds array

resource assignments 'Microsoft.Authorization/policyAssignments@2024-04-01' = [
  for id in definitionIds: {
    name: take(last(split(id, '/')), 64)
    properties: {
      policyDefinitionId: id
      enforcementMode: 'Default'
    }
  }
]
