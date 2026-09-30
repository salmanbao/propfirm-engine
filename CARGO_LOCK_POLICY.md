# Cargo.lock Policy

This document describes how `Cargo.lock` is managed in the propfirm-engine
repository.

## TL;DR

- `Cargo.lock` is **committed** to the repository.
- Run `cargo update` only when intentionally upgrading dependencies.
- For security-critical upgrades, use `cargo update -p <crate>`.
- The `outdated` CI job (weekly) reports available upgrades.
- The `security-audit` CI job (every PR + nightly) blocks on RUSTSEC advisories.

## Why Cargo.lock is committed

The propfirm-engine is a **binary** (not a library that other crates depend
on). For binaries, committing `Cargo.lock` ensures:

1. **Reproducible builds** — the exact same dependency versions are used
   every time, everywhere. No "works on my machine but not in CI."
2. **Auditable supply chain** — the lockfile records the exact version +
   source (crates.io hash) of every transitive dependency. Combined with
   the CycloneDX SBOM in the Docker image, this provides full provenance.
3. **Faster CI** — `cargo build` doesn't need to re-resolve the dependency
   graph from scratch on every run. The lockfile pins the versions, so
   only compilation (not resolution) happens.
4. **Controlled upgrades** — `cargo update` is a deliberate action, not
   an accidental side effect of `cargo build`.

## When to run `cargo update`

### Routine upgrades (weekly)

The `outdated` CI job runs every Sunday at 06:00 UTC. It reports:

- Which direct + transitive deps have newer compatible versions available.
- Which deps have newer major versions (breaking changes).

**Action**: review the report, then run `cargo update` to pull in the
latest compatible versions. Run `cargo test --all-features` after. If
all tests pass, commit the updated `Cargo.lock`.

### Security upgrades (immediately)

When the `security-audit` CI job reports a RUSTSEC advisory:

1. Read the advisory at https://rustsec.org/advisories/<ID>.html.
2. Determine if it affects our use case (read the "affected versions"
   and "unsound" sections).
3. If it does, upgrade the affected crate immediately:
   ```bash
   cargo update -p <affected-crate>
   cargo test --all-features
   ```
4. If the upgrade requires a major version bump (breaking changes), see
   "Major version upgrades" below.
5. If the advisory is informational and doesn't affect our use case,
   add it to `deny.toml`'s `[advisories].ignore` list with a comment
   explaining why.

### Major version upgrades (case-by-case)

When a dep has a new major version (e.g., `tokio` 1.x → 2.x):

1. Read the crate's CHANGELOG / migration guide.
2. Create a branch: `git checkout -b upgrade-<crate>-<new-major>`.
3. Update `Cargo.toml`: `<crate> = "<new-major>"`.
4. Run `cargo update -p <crate>`.
5. Fix any compile errors (the new API may differ).
6. Run `cargo test --all-features`.
7. Run `cargo clippy --all-features --all-targets -- -D warnings`.
8. Open a PR with the upgrade + a summary of the breaking changes.

## What NOT to do

- **Do NOT** run `cargo update` in a PR that also changes source code.
  Keep dependency upgrades and feature changes in separate PRs so the
  reviewer can focus on one thing at a time.
- **Do NOT** delete `Cargo.lock`. Ever. If you accidentally delete it,
  run `cargo generate-lockfile` to regenerate it, then `cargo test
  --all-features` to verify.
- **Do NOT** edit `Cargo.lock` by hand. Use `cargo update` / `cargo add`
  / `cargo remove` instead.
- **Do NOT** add `Cargo.lock` to `.gitignore`. It should always be
  committed for binary crates.

## How the CI jobs enforce this

| CI Job | Frequency | What it checks |
|---|---|---|
| `check` | Every PR | `cargo test --all-features` (uses committed lockfile) |
| `security-audit` | Every PR + nightly | `cargo audit` — RUSTSEC advisory scan |
| `cargo-deny` | Every PR + nightly | `cargo deny check` — advisories + licenses + bans + sources |
| `outdated` | Weekly | `cargo outdated` — available upgrades report |
| `sbom` | Every PR | `cargo cyclonedx` — generates SBOM from lockfile |
| `nextest` | Every PR | `cargo nextest run --all-features` (uses committed lockfile) |

## Related files

- `Cargo.lock` — the committed lockfile
- `deny.toml` — cargo-deny config (advisories, licenses, bans, sources)
- `.config/nextest.toml` — test runner config
- `.github/workflows/ci.yml` — CI pipeline
- `.github/dependabot.yml` — automated dep upgrade PRs
