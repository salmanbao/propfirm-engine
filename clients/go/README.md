# propfirm-engine Go client

A typed Go client for the propfirm-engine HTTP API.

## Install

```bash
go get github.com/salmanbao/propfirm-engine/clients/go
```

## Usage

```go
package main

import (
    "context"
    "fmt"
    "log"

    propfirm "github.com/salmanbao/propfirm-engine/clients/go"
)

func main() {
    // Create a client pointing at the engine.
    client := propfirm.New("http://propfirm-server:8080", "my-tenant-uuid")

    // Check health.
    if err := client.Health(context.Background()); err != nil {
        log.Fatalf("engine not healthy: %v", err)
    }

    // Submit an evaluation.
    resp, err := client.EvaluateWithAutoKey(context.Background(), &propfirm.EvaluateRequest{
        AccountID: "uuid-here",
        AccountState: json.RawMessage(`{...}`), // full account state
        BridgeTick: &propfirm.BridgeTick{
            Payload: propfirm.BridgeTickPayload{
                EquityCents:  10000000,
                BalanceCents: 10000000,
                BrokerTime:    time.Now().UnixMilli(),
                Positions:     []propfirm.PositionDTO{},
            },
        },
    })
    if err != nil {
        log.Fatalf("evaluate failed: %v", err)
    }

    fmt.Printf("decision=%s hash=%s\n", resp.DecisionKind, resp.InputHash)
}
```

## API methods

| Method | Endpoint | Purpose |
|---|---|---|
| `Health` | `GET /health` | Liveness probe |
| `Ready` | `GET /ready` | Readiness probe |
| `Metrics` | `GET /metrics` | Prometheus scrape |
| `Evaluate` | `POST /internal/v1/evaluate` | Stateless evaluation (with Idempotency-Key) |
| `EvaluateWithAutoKey` | `POST /internal/v1/evaluate` | Same, auto-generates UUID key |
| `EvaluateOrder` | `POST /v1/evaluate-order` | Pre-trade order evaluation |
| `Override` | `POST /internal/v1/override` | Clear a false-positive breach |
| `EmergencyStop` | `POST /internal/v1/emergency-stop` | Force emergency stop |
| `ValidateRulePack` | `POST /v1/rule-packs/validate` | Validate a rule pack |

## Configuration

The client uses a default 30-second HTTP timeout. Override with
`NewWithHTTPClient`:

```go
client := propfirm.NewWithHTTPClient(
    "http://propfirm-server:8080",
    "tenant-uuid",
    &http.Client{
        Timeout: 10 * time.Second,
        Transport: &http.Transport{
            MaxIdleConns:        100,
            MaxIdleConnsPerHost: 10,
        },
    },
)
```
