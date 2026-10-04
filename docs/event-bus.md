# Event Bus (D81 Stateless Design)

**Important**: As of v0.2.0, the propfirm-engine no longer includes a worker binary or built-in event bus consumer. The engine emits `DomainEvent` objects that must be consumed by the platform's event processing system.

## Overview

In the D81 stateless design, the engine's responsibility is limited to pure evaluation. Event consumption and processing is handled entirely by the platform.

## Engine Responsibilities

The engine emits `DomainEvent` objects in `PipelineResult.events` for:
- All state transitions (account status changes, position updates, etc.)
- Rule violations (for audit and alerting)
- Special events (emergency stops, overrides, payouts, etc.)

These events are:
- **Append-only**: Represent facts that occurred at a specific point in time
- **Causally linked**: Include causation IDs to trace event chains
- **Complete**: Contain all information needed to rebuild state or understand transitions
- **Immutable**: Never modified after emission

## Platform Responsibilities

The platform must implement an event processing system that:

### Consumes Engine Events
- Subscribes to the engine's event emission mechanism (HTTP responses, message queue, etc.)
- Validates event signatures and integrity if required
- Processes events in order of occurrence for each account

### Stores Events Persistently
- Appends events to a durable event log (technology of platform's choice)
- Ensures events are not lost (replication, acknowledgments, etc.)
- Provides querying capabilities for breach reports, audits, and analytics

### Provides Replay Capability
- Can reconstruct `account_state` at any point by replaying events
- Handles event schema evolution gracefully
- Supports rebuilding state for new platform instances

### Handles Event Processing
- Applies event effects to build current `account_state`
- Detects and handles duplicate events (if using at-least-once delivery)
- Manages event partitioning/sharding for scalability

## Recommended Implementation Patterns

### Event Storage Technologies
- **Append-only logs**: Apache Kafka, AWS Kinesis, Pulsar
- **Event stores**: EventStoreDB, Greg's Log
- **Databases**: PostgreSQL with JSONB, MongoDB, Cassandra
- **Object storage**: Apache Parquet files in data lakes (for analytics)

### Processing Patterns
- **Stream processing**: Apache Flink, Storm, Spark Structured Streaming
- **Simple consumers**: Language-specific clients with checkpointing
- **Batch processing**: Periodic recomputation from scratch (for smaller scales)

### Guarantees to Aim For
- **At-least-once delivery**: Ensures no events are lost (engine can emit duplicates if needed)
- **Idempotent processing**: Processing the same event multiple times has the same effect as once
- **Ordered processing per account**: Events for a single account are processed in `occurred_at` order

## Event Types Emitted

The engine emits these `DomainEventKind` varieties:

1. **Account Lifecycle**
   - `AccountStarted` - Initial account creation
   - `AccountStatusChanged` - Status transitions (Active → Failed, etc.)
   - `DayRollover` - Trading day boundary

2. **Trading Activity**
   - `OrderSubmitted` - Pre-trade order validation request
   - `TradeFilled` - Post-trade state update
   - `TickEvaluated` - Market tick evaluation

3. **Rule Outcomes**
   - `RuleViolated` - Individual rule violations (warnings, failures, etc.)
   - `PayoutRequested` / `PayoutApproved` - Payout lifecycle events
   - `OverrideCleared` - Manual override of false positive breach
   - `EmergencyStop` - Emergency stop requested

4. **Special Events**
   - `PlanUpgraded` - Account moved to higher challenge phase
   - `LiquidationRequested` - Position liquidation required

## Implementation Example (Conceptual)

```python
# Platform event consumer pseudocode
class PropFirmEventConsumer:
    def __init__(self, event_store, state_store):
        self.event_store = event_store
        self.state_store = state_store
        self.account_states = {}  # In-memory cache of current states
    
    async def process_event(self, event: DomainEvent):
        # 1. Persist the event
        await self.event_store.append(event)
        
        # 2. Update account state if applicable
        if event.affects_account_state():
            current_state = self.state_store.get(event.account_id) or Account.initial()
            new_state = event.apply_to_state(current_state)
            self.state_store.save(event.account_id, new_state)
            self.account_states[event.account_id] = new_state
        
        # 3. Handle special event types
        if event.kind == DomainEventKind.RuleViolated:
            await self.handle_violation(event)
        elif event.kind == DomainEventKind.PayoutRequested:
            await self.handle_payout_request(event)
    
    async def replay_account(self, account_id: UUID) -> Account:
        """Reconstruct account state from event log"""
        events = await self.event_store.get_all_for_account(account_id)
        state = Account.initial()
        for event in sorted(events, key=lambda e: (e.occurred_at, e.inserted_at)):
            if event.affects_account_state():
                state = event.apply_to_state(state)
        return state
```

## Integration with Engine

When the platform calls the engine:
1. Platform retrieves current `account_state` from its storage
2. Platform builds `RuleContext` from the `account_state` and incoming event data
3. Platform calls engine evaluation function (`evaluate_internal`, `evaluate_order`, etc.)
4. Engine returns:
   - Updated `account_state` (platform stores this)
   - `DomainEvent` objects (platform persists and processes these)
   - Evaluation result (for immediate response to caller)
5. Platform acknowledges completion to any message broker (if using queues)

## Benefits of This Approach

1. **Clear Separation of Concerns**: Engine focuses on evaluation, platform focuses on reliability
2. **Technology Choice Freedom**: Platform can use best-fit technologies for each concern
3. **Operational Simplicity**: Engine has no external dependencies beyond its callers
4. **Enhanced Observability**: Platform can implement rich event streaming analytics
5. **Improved Fault Isolation**: Engine failures don't corrupt persistent event stores
