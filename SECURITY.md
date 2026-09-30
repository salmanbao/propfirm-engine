# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.2.x   | ✅        |
| < 0.2   | ❌        |

## Reporting a Vulnerability

The propfirm-engine team takes security vulnerabilities seriously.
We appreciate your efforts to responsibly disclose your findings.

### How to report

**DO NOT open a public GitHub issue for security vulnerabilities.**

Instead, report via one of these channels:

1. **GitHub Security Advisory** (preferred):
   - Go to https://github.com/salmanbao/propfirm-engine/security/advisories/new
   - Click "Report a vulnerability"
   - Fill in the details (affected version, reproduction steps, impact)

2. **Email** (if GitHub is not available):
   - Send to: `security@propfirm.example`
   - Use PGP if possible (key fingerprint: `TBD`)

### What to include in your report

- **Description** of the vulnerability and its impact.
- **Affected version(s)** — run `propfirm-server --version` or check
  `Cargo.toml`.
- **Reproduction steps** — a minimal test case or curl command that
  demonstrates the issue.
- **Potential mitigations** you've considered (if any).

### Response timeline

| Step | Target | Description |
|------|--------|-------------|
| Acknowledgment | 24 hours | We confirm receipt of your report |
| Initial assessment | 72 hours | We assess severity + feasibility |
| Fix timeline | 7-30 days | Depending on severity + complexity |
| Disclosure | 90 days | Or after a fix is released, whichever comes first |

## Scope

### In scope

- The propfirm-engine Rust codebase (`src/`).
- The HTTP API (`/internal/v1/*`, `/v1/*`).
- The event-bus worker binary (`propfirm-worker`).
- The Docker image (`Dockerfile`).
- The Helm chart (`deploy/helm/`).
- Dependency vulnerabilities (RUSTSEC advisories affecting our deps).

### Out of scope

- The platform backend that calls the engine (not part of this repo).
- The Redis / Postgres / OTLP collector infrastructure (managed by
  the operator, not by this repo).
- Social engineering, physical access, DDoS.

## Security measures already in place

- **No authentication** — the engine is internal-only. Network-level
  trust (private compose network, k8s NetworkPolicy) is the security
  boundary. See `deploy/helm/templates/networkpolicy.yaml`.
- **TLS termination** — optional in-process rustls with optional mTLS
  (client cert verification). See `docs/tls.md`.
- **Input validation** — all request bodies are validated via serde
  deserialization. Invalid JSON returns 400 with a stable error shape
  (no implementation details leaked). See `src/api/middleware.rs`.
- **Panic safety** — `RuleRegistry::evaluate` wraps each rule in
  `catch_unwind`. A buggy rule degrades to `Warn`, not a crash. See
  `src/rules/registry.rs`.
- **Decimal money** — all monetary values use `rust_decimal::Decimal`.
  No floating-point drift on money. See `src/core/types.rs`.
- **Audit log** — every sensitive action (override, emergency-stop,
  worker evaluation) writes to the `audit_log` table. See
  `src/api/audit_log.rs`.
- **Supply-chain security** — `cargo audit` (RUSTSEC), `cargo deny`
  (advisories + licenses + bans + sources), CycloneDX SBOM embedded in
  the Docker image. See `deny.toml` + `Dockerfile`.

## Disclosure policy

- We follow **coordinated disclosure**. We will not publicly disclose
  a vulnerability until a fix is available, or 90 days have passed
  since the report (whichever comes first).
- We will credit you in the release notes + `CHANGELOG.md` unless you
  prefer to remain anonymous.

## Security checks in CI

| CI Job | Frequency | What it does |
|---|---|---|
| `security-audit` | Every PR + nightly | `cargo audit` — scans `Cargo.lock` against RUSTSEC |
| `cargo-deny` | Every PR + nightly | `cargo deny check` — advisories + licenses + bans + sources |
| `sbom` | Every PR | `cargo cyclonedx` — generates CycloneDX SBOM |
| `fuzz` | Nightly | 60s fuzz runs (panic-safety + determinism) |
| `chaos` | Every PR + nightly | Redis kill + recovery test |

## Contact

- Security: `security@propfirm.example`
- General issues: https://github.com/salmanbao/propfirm-engine/issues
