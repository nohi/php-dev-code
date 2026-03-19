# AI Team Process

## Roles

- Architect Agent:
  - Owns feature decomposition and protocol contracts.
  - Maintains architecture decision records and milestone scope.

- Implementer Agent:
  - Delivers code in small, reviewable changesets.
  - Adds tests and benchmark updates with each major capability.

- Reviewer Agent:
  - Focuses on correctness, performance risk, memory impact, and compatibility.
  - Blocks merges without test evidence and benchmark checks for core paths.

## Workflow

1. Architect defines a task spec and acceptance criteria.
2. Implementer creates a branch and delivers the minimal complete slice.
3. Reviewer runs static checks, tests, benchmark subset, and code review.
4. If rejected, implementer revises and re-requests review.
5. If approved, merge with changelog and milestone tracking update.

## Pull Request Checklist

- Feature behavior matches acceptance criteria.
- Extension <-> server protocol changes versioned and documented.
- Unit/integration tests added or updated.
- Benchmark result included when touching analysis/completion/indexing.
- Cross-platform considerations stated.

## Definition of Done

- Code merged and CI green.
- Tests and docs updated.
- Known limitations recorded.
- Next tasks queued with clear owner role.
