// A static share, not a distributed lease: reserve capacity for every possible
// API replica even when fewer are running. Never round up past the budget.
@export()
func replicaComputeSlots(budget int, replicas int, shared bool) int =>
  budget < 1 || replicas < 1
    ? int('Computation budget and API max replicas must be positive')
    : (shared
      ? (budget < replicas
        ? int('Shared searcher computation budget must be at least API max replicas')
        : budget / replicas)
      : budget)
