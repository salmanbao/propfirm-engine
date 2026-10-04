# Persistence (D81 Stateless Design)

**Important**: As of v0.2.0, the propfirm-engine follows the D81 stateless compute service design. **No persistence layers exist within the engine itself.** All state ownership, ordering, idempotency, retry, and dead letter queue (DLQ) handling is the responsibility of the platform's `workers` consumer.

## Overview

The engine is a pure function that transforms inputs to outputs:
- **Input**: `account_state` (provided in request) + `RuleContext` built from event data
- **Output**: Updated `account_state` (returned in response) + `DomainEvent` emissions

All persistence concerns are handled externally by the platform:
- Idempotency: Handled by the platform before sending requests to the engine
- Event sourcing: The platform consumes `DomainEvent` emissions from the engine and stores them in its preferred event store
- Audit trail: The platform owns and manages the audit log (the engine only emits events)
- State storage: The platform stores and versions `account_state` between engine evaluations

This design eliminates a major source of complexity and failure modes while improving horizontal scalability.

## Engine Responsibilities

The engine focuses exclusively on pure evaluation:

1. **Stateless Evaluation**: Each evaluation is independent - no server-side state is retained between requests
2. **Deterministic Replay**: Given the same inputs (`account_state`, `RuleContext`), the engine produces identical outputs
3. **Input Hashing**: The `input_hash` field in responses enables byte-for-byte recomputation of past verdicts
4. **Event Emission**: The engine outputs `DomainEvent` objects that describe state transitions
5. **Return State**: The updated `account_state` is returned in the response for the platform to store

## Platform Responsibilities

The platform must handle:

### Idempotency
- Deduplicate mutating HTTP requests before sending to the engine
- Common approaches: Redis-based idempotency with TTL, database unique constraints, or idempotency keys with expiration
- The engine's `input_hash` can be used as part of the idempotency key

### Event Storage
- Consume `DomainEvent` objects from engine responses
- Store them in an append-only event log (the platform's choice of technology)
- Provide replay capability to reconstruct account state from events

### State Management
- Store versions of `account_state` between engine evaluations
- Handle optimistic concurrency conflicts (return `409 Conflict` when versions don't match)
- Seed initial `account_state` from `AccountStarted` events

### Audit Trail
- Own and manage the complete audit log
- Replay engine-emitted events to build the audit trail
- The engine has no direct database connection for audit writes (D81/I-25)

## Evaluation Flow (D81)

```
Platform → Engine → Platform
  │         │         │
  │         ▼         │
  │   Evaluate    │
  │         │         │
  ▼         ▼         ▼
Request → [Engine] → Response
  │         │         │
  │  account_state    │
  │   (from storage)  │
  │         │         │
  │         ▼         │
  │   RuleContext     │
  │         │         │
  │         ▼         │
  │   Pure Evaluation │
  │         │         │
  │         ▼         │
  │   Decision +      │
  │   Updated State   │
  │         │         │
  │         ▼         │
  │   DomainEvents    │
  │         │         │
  ▼         ▼         ▼
Response ← [Engine] ← Events
  │         │         │
  │  account_state    │
  │   (to storage)    │
  │         │         │
  ▼         ▼         ▼
Storage ← Platform → Message Broker
  │         │         │
  │  Store state    │  Emit events
  │  Handle idempotency  to platform consumers
  │  Build audit trail   │
  ▼         ▼         ▼
```

## Key Benefits

1. **Horizontal Scalability**: Engine instances can be freely scaled since no shared state exists
2. **Failure Isolation**: Engine crashes don't corrupt persistent state
3. **Simplified Reasoning**: Each evaluation is a pure function
4. **Technology Flexibility**: Platform can choose optimal storage technologies for each concern
5. **Operational Simplicity**: No database schema migrations, connection pooling, or backup/restore procedures for the engine itself

## Migration Notes

If migrating from a stateful design to this stateless approach:
1. Remove all persistence dependencies from the engine layer
2. Move idempotency checks to the platform layer before calling the engine
3. Implement event sourcing where the platform consumes `DomainEvent` emissions
4. Design platform-side state storage with versioning for optimistic concurrency
5. Have the platform own and manage the audit trail by replaying engine events

The engine now focuses exclusively on what it does best: pure, deterministic rule evaluation.
