# Helm chart for propfirm-engine

Production-grade Kubernetes deployment of the propfirm-engine:
- HTTP server (`propfirm-server`) — 3 replicas, autoscaled, behind a
  `Service` + optional `Ingress`
- Event bus worker (`propfirm-worker`) — 3 replicas, autoscaled,
  consumes from Redis Streams
- Postgres StatefulSet (optional, when `postgres.deploy=true`)
- Redis StatefulSet (optional, when `redis.deploy=true`)
- HorizontalPodAutoscaler for both server and worker
- PodDisruptionBudget (min 2 available at all times)
- NetworkPolicy (restricts ingress to specific namespaces, egress to
  infra namespace)
- ServiceMonitor for the Prometheus Operator (optional)
- ConfigMap + Secret templates for all settings

## Quick start

```bash
# From the repo root:
helm install propfirm ./deploy/helm \
  --namespace propfirm \
  --create-namespace

# Verify:
kubectl -n propfirm get pods
kubectl -n propfirm port-forward svc/propfirm-server 8080:80
curl http://localhost:8080/health   # → ok
```

## Values

All defaults are in [`values.yaml`](values.yaml). Override via
`--values custom.yaml` or `--set key=value`:

```bash
# External Postgres + Redis (don't deploy them):
helm install propfirm ./deploy/helm \
  --namespace propfirm \
  --set postgres.deploy=false \
  --set postgres.externalDsn=postgresql://user:pass@my-postgres:5432/propfirm \
  --set redis.deploy=false \
  --set redis.externalUrl=redis://my-redis:6379

# Enable TLS (in-process rustls, requires the cert secret):
kubectl create namespace propfirm
kubectl -n propfirm create secret tls propfirm-tls \
  --cert=cert.pem --key=key.pem
helm install propfirm ./deploy/helm \
  --namespace propfirm \
  --set server.tls.enabled=true \
  --set server.tls.certSecret=propfirm-tls

# Scale up the worker pool for high event-bus throughput:
helm upgrade propfirm ./deploy/helm \
  --namespace propfirm \
  --set worker.replicaCount=10 \
  --set worker.concurrency=32

# Use a private registry image:
helm install propfirm ./deploy/helm \
  --set image.repository=my-registry.io/propfirm-engine \
  --set image.tag=v0.2.0 \
  --set image.pullSecrets[0].name=registry-pull-secret
```

## Architecture

```
                ┌────────────────────────────────────────────────┐
                │           Platform backend (caller)            │
                │      (namespace: platform-backend)              │
                └─────────┬────────────────────────┬────────────┘
                          │ HTTP POST              │ XADD to
                          │ /internal/v1/evaluate  │ request stream
                          ▼                        ▼
       ┌──────────────────────────┐  ┌──────────────────────────┐
       │  propfirm-server (3+)     │  │  propfirm-worker (3+)     │
       │  (Deployment + HPA)       │  │  (Deployment + HPA)      │
       │  axum + rustls + tower    │  │  Redis Streams consumer   │
       │  /metrics endpoint        │  │  N concurrent tasks       │
       └──────────┬───────────────┘  └──────────┬───────────────┘
                  │                              │
                  └──────────────┬───────────────┘
                                 ▼
                   ┌─────────────────────────────┐
                   │ infrastructure namespace      │
                   │  ┌────────────┐ ┌─────────┐  │
                   │  │ Postgres    │ │ Redis   │  │
                   │  │ StatefulSet │ │ Stateful│  │
                   │  │ (events,    │ │ Set     │  │
                   │  │  idempotency│ │ (cache, │  │
                   │  │  audit_log) │ │  bus)   │  │
                   │  └────────────┘ └─────────┘  │
                   └─────────────────────────────┘
```

The NetworkPolicy restricts:
- Ingress: only pods in `networkPolicy.ingressFromNamespaces`
  (default: `platform-backend`) can reach the server.
- Egress: only DNS to `kube-system` + pods in
  `networkPolicy.egressToNamespaces` (default: `infrastructure`).

The worker has NO inbound traffic — it only consumes from Redis.

## Verifying the chart locally

```bash
# Lint the chart (requires helm):
helm lint ./deploy/helm

# Render the templates (dry-run, doesn't apply):
helm template propfirm ./deploy/helm --namespace propfirm | less

# Use kind/minikube for local k8s testing:
kind create cluster --name propfirm-dev
helm install propfirm ./deploy/helm --namespace propfirm --create-namespace
kubectl -n propfirm get pods -w

# Once pods are Ready:
kubectl -n propfirm port-forward svc/propfirm-server 8080:80
curl http://localhost:8080/health    # → ok
curl http://localhost:8080/ready     # → ready
curl http://localhost:8080/metrics   # → Prometheus scrape
```

## CI/CD integration

```bash
# In your pipeline:
- name: Lint Helm chart
  run: helm lint ./deploy/helm

- name: Render templates
  run: helm template propfirm ./deploy/helm > rendered.yaml

- name: Deploy to staging
  run: |
    helm upgrade --install propfirm ./deploy/helm \
      --namespace propfirm-staging \
      --create-namespace \
      --values deploy/helm/values.staging.yaml \
      --wait --timeout 5m

- name: Smoke test staging
  run: |
    kubectl -n propfirm-staging port-forward svc/propfirm-server 8080:80 &
    sleep 2
    curl -fsS http://localhost:8080/health
    curl -fsS http://localhost:8080/ready
```

## Production checklist

Before deploying to production, verify:

- [ ] `postgres.deploy=false` pointing at your managed Postgres (RDS,
      Cloud SQL, Aurora) — don't run Postgres in a StatefulSet.
- [ ] `redis.deploy=false` pointing at your managed Redis (ElastiCache,
      MemoryStore, Upstash) — same reason.
- [ ] `server.tls.enabled=true` with a cert from `cert-manager`.
- [ ] `networkPolicy.enabled=true` and the ingress/egress namespace
      lists are correct.
- [ ] `observability.serviceMonitor.enabled=true` (if you use the
      Prometheus Operator).
- [ ] `observability.otlp.endpoint` set to your OpenTelemetry collector.
- [ ] `idempotency.backend=redis` (not `memory`).
- [ ] `worker.replicaCount >= 3` for HA (one pod can be down without
      losing consumer liveness).
- [ ] `server.pdb.minAvailable >= 2` and `worker.pdb.minAvailable >= 2`.
- [ ] Resource requests match your actual workload (run the k6 load
      test in `bench/load/` against staging first).
- [ ] The image is built with `--features server,otel` if you need OTLP.
