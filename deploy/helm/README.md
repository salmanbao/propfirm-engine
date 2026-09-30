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

### Infrastructure
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

### Worker health
- [ ] `worker` livenessProbe + readinessProbe wired to
      `/app/propfirm-worker healthcheck` (default in the chart since v0.3).
      Verify with: `kubectl -n propfirm exec deploy/propfirm-worker --
      /app/propfirm-worker healthcheck` — should print "OK: redis
      reachable, stream '...' present, group '...' registered".

### Supply-chain security
- [ ] `cargo audit` passes in CI (the `security-audit` job). The
      latest run's advisory count is visible at the top of the
      workflow run page on GitHub Actions. Any RUSTSEC advisory on a
      direct or transitive dep blocks the merge.
- [ ] `cargo deny check advisories` passes in CI (the `cargo-deny`
      job). This catches the same advisories as `cargo audit` plus
      yanked crates.
- [ ] `cargo deny check bans` passes — no banned crates (e.g.
      copybara-fork, openssl pre-1.1.1k).
- [ ] `cargo deny check licenses` passes — every direct + transitive
      dep is on the allowlist (default: MIT, Apache-2.0, BSD-3-Clause,
      BSD-2-Clause, ISC, MPL-2.0).
- [ ] The Docker image carries a CycloneDX SBOM at `/app/sbom/`. Verify
      with: `docker run --rm ghcr.io/salmanbao/propfirm-engine:latest
      cat /app/sbom/*.json | jq .metadata`.
- [ ] The image's OCI labels include `io.propfirm.sbom.location` —
      `syft` / `trivy` / `grype` can read this to discover the SBOM
      without rescanning the filesystem.

### Observability
- [ ] The Grafana dashboard at `deploy/helm/dashboards/propfirm-overview.json`
      is imported into your Grafana instance (or use the
      `grafana_dashboard` ConfigMap annotation for auto-import with the
      Grafana Helm chart's sidecar).
- [ ] The OTLP collector (Tempo / Jaeger / Honeycomb / Datadog) is
      receiving spans from `propfirm-engine` — verify by emitting a
      test span: `kubectl -n propfirm exec deploy/propfirm-server --
      /app/propfirm-server` and watching the collector's `/metrics`
      endpoint for `otelcol_receiver_accepted_spans` increments.
