# Helm chart for propfirm-engine

D81: Production-grade Kubernetes deployment of the propfirm-engine as a
**stateless compute service**:

- HTTP server (`propfirm-server`) — 3 replicas, autoscaled, behind a
  `Service` + optional `Ingress`
- HorizontalPodAutoscaler for the server
- PodDisruptionBudget (min 2 available at all times)
- NetworkPolicy (restricts ingress to specific namespaces)
- ServiceMonitor for the Prometheus Operator (optional)
- ConfigMap for all settings

**No Postgres, no Redis, no worker.** The platform's `workers` consumer
(a separate Go service in the `alpha-one` repo) owns all state, ordering,
idempotency, retry, and DLQ (docs/64 §4.1).

## Architecture

```
┌───────────────────────────────────────────────────────────────┐
│                    Platform `workers` (Go)                    │
│  (separate repo — owns evaluation_state, consumer_state, DLQ) │
└────────────┬──────────────────────────────────────────────────┘
             │ HTTP (service token, private network)
             ▼
┌───────────────────────────────────────────────────────────────┐
│              propfirm-server (3+ replicas)                     │
│  Stateless compute: (state, plan, tick) → (verdict, hints)   │
│  No DB · No Redis · No state · Scales by replica count       │
└───────────────────────────────────────────────────────────────┘
```

## Install

```bash
helm install propfirm ./deploy/helm \
  --set server.replicaCount=3
```

## Verify

```bash
kubectl port-forward svc/propfirm-server 8080:80
curl http://localhost:8080/health   # → ok
curl http://localhost:8080/ready    # → ready (no DB dependency)
curl http://localhost:8080/metrics   # → Prometheus format
```

## Production checklist

- [ ] `server.replicaCount >= 3` for HA (one pod can be down without
      downtime).
- [ ] `server.hpa.enabled = true` (autoscaled 3–20 based on CPU).
- [ ] `server.pdb.minAvailable >= 2`.
- [ ] `networkPolicy.ingressFromNamespaces` updated to include the
      platform's `workers` namespace.
- [ ] TLS enabled (`server.tls.enabled = true`) with cert/key mounted
      from a Secret, OR behind an ingress that terminates TLS.
- [ ] OTLP exporter configured (`observability.otlp.endpoint`) if
      distributed tracing is desired.

## Configuration

The engine has only two config sections: `server` and `observability`.
No `postgres`, `redis`, `idempotency`, or `eventBus` sections exist
(D81 — the engine is stateless).

| Key | Default | Description |
|-----|---------|-------------|
| `server.bindAddr` | `0.0.0.0:8080` | HTTP bind address |
| `server.replicaCount` | `3` | Number of server replicas |
| `server.hpa.enabled` | `true` | Enable HorizontalPodAutoscaler |
| `server.hpa.minReplicas` | `3` | Minimum replicas |
| `server.hpa.maxReplicas` | `20` | Maximum replicas |
| `server.tls.enabled` | `false` | Enable in-process TLS (rustls) |
| `observability.logFilter` | `info,propfirm=debug` | Log filter |
| `observability.logFormat` | `json` | Log format (json \| pretty) |
| `observability.metricsEnabled` | `true` | Expose /metrics endpoint |
| `observability.otlp.endpoint` | `""` | OTLP collector endpoint (empty = disabled) |

For more details: https://github.com/salmanbao/propfirm-engine
