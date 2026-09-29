# TLS Termination

The propfirm-engine supports **in-process TLS termination** via
`rustls`. This is appropriate when:

- You want end-to-end encryption on the private compose network
  (defense in depth against network misconfiguration).
- You're running mTLS to the platform bridge (each service has its
  own cert).
- You have no external TLS-terminating proxy (envoy / nginx / AWS ALB).

When TLS is **disabled** (default), the server runs plain HTTP —
appropriate when behind an external proxy.

## Configuration

```toml
[server.tls]
enabled = true
cert_path = "/etc/propfirm/tls/cert.pem"
key_path = "/etc/propfirm/tls/key.pem"
```

Env-var equivalent:

```bash
PROPFIRM_SERVER__TLS__ENABLED=true
PROPFIRM_SERVER__TLS__CERT_PATH=/etc/propfirm/tls/cert.pem
PROPFIRM_SERVER__TLS__KEY_PATH=/etc/propfirm/tls/key.pem
```

## Certificate formats

- **Cert**: PEM-encoded, leaf first, then intermediates (if any).
- **Key**: PEM-encoded, PKCS#8 or PKCS#1 (RSA).

The server uses `axum-server` with `tls-rustls`, which internally uses
`rustls-pemfile` to parse the PEM input. ECDSA and RSA keys are both
supported.

## Generating a self-signed cert (for dev / testing)

```bash
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout /tmp/key.pem -out /tmp/cert.pem -days 365 \
  -subj '/CN=localhost' \
  -addext 'subjectAltName=DNS:localhost,IP:127.0.0.1'

# Run with TLS:
PROPFIRM_SERVER__TLS__ENABLED=true \
PROPFIRM_SERVER__TLS__CERT_PATH=/tmp/cert.pem \
PROPFIRM_SERVER__TLS__KEY_PATH=/tmp/key.pem \
cargo run --release --features server --bin propfirm-server

# Verify (self-signed needs --insecure):
curl --insecure https://localhost:8080/health
# Expected: ok
```

## Generating a Let's Encrypt cert (production)

Use `certbot` with the DNS challenge (the engine doesn't serve HTTP-01
challenges well because it's a private service):

```bash
certbot certonly --dns-cloudflare --dns-cloudflare-credentials ~/.secrets/cloudflare.ini \
  -d propfirm.internal.example.com
# Outputs:
#   /etc/letsencrypt/live/propfirm.internal.example.com/fullchain.pem
#   /etc/letsencrypt/live/propfirm.internal.example.com/privkey.pem
```

Then:

```bash
PROPFIRM_SERVER__TLS__ENABLED=true \
PROPFIRM_SERVER__TLS__CERT_PATH=/etc/letsencrypt/live/.../fullchain.pem \
PROPFIRM_SERVER__TLS__KEY_PATH=/etc/letsencrypt/live/.../privkey.pem \
/app/propfirm-server
```

## mTLS (mutual TLS) — for service-to-service auth

mTLS is **not yet wired** in v0.2.0. When you need it (e.g., to bind
a service cert to the platform bridge), you'll need to:

1. Add `ClientConfig` with a `root_cert_store` containing the
   platform's CA.
2. Configure `rustls::ServerConfig` with
   `with_client_cert_verifier(AllowAnyAuthenticatedClients)`.
3. Pass the resulting `ServerConfig` to `axum_server` instead of the
   `RustlsConfig::from_pem` shortcut.

Sample code (when this is wired):

```rust
use rustls::server::{ServerConfig, AllowAnyAuthenticatedClients};
use rustls::RootCertStore;

let mut root_store = RootCertStore::empty();
for cert_pem in std::fs::read("/etc/propfirm/tls/ca.pem")? {
    root_store.add(cert_pem)?;
}
let server_config = ServerConfig::builder()
    .with_client_cert_verifier(AllowAnyAuthenticatedClients::new(root_store))
    .with_single_cert(certs, key)?;
let tls_config = axum_server::tls_rustls::RustlsConfig::from_config(server_config);
```

## Recommended deployment topologies

### Topology A — in-process TLS, no external proxy

```
[ platform bridge ] --TLS(mTLS?)--> [ propfirm-server:8080 ]
```

Use when:
- Private network only
- Single ingress point
- You want defense-in-depth against misconfigured network policies

Config: `tls.enabled = true`

### Topology B — plain HTTP behind external proxy

```
[ platform bridge ] --TLS--> [ envoy/nginx/k8s-ingress ] --HTTP--> [ propfirm-server:8080 ]
```

Use when:
- You already operate a TLS-terminating gateway
- You need cert-manager / Let's Encrypt automation
- Multiple services behind the same gateway

Config: `tls.enabled = false`

### Topology C — mTLS to platform bridge

```
[ platform bridge (with client cert) ] --mTLS--> [ propfirm-server:8080 ]
```

Use when:
- You need cryptographic client identity verification
- Defense in depth against compromised network credentials

Config: `tls.enabled = true` + mTLS wired (TODO above)

## Verification

After enabling TLS:

1. Check the server logs show `tls_enabled=true`:

   ```bash
   docker compose logs propfirm-server | grep tls_enabled
   ```

2. `curl --insecure https://localhost:8080/health` returns `ok`.

3. Without `--insecure`, you get a certificate verification error
   (expected for self-signed certs in dev).

4. For Let's Encrypt certs:

   ```bash
   curl https://propfirm.internal.example.com/health
   # Expected: ok (no --insecure needed)
   ```

## Cipher suite selection

The `rustls` default cipher suite list is curated by the Rust
security team and is the recommended set. We do NOT override it.

If your compliance regime requires a specific cipher list, you'll
need to fork `axum_server::tls_rustls::RustlsConfig` — out of scope
for v0.2.0.

## Common issues

### "TLS cert read failed"

The path in `cert_path` doesn't exist or isn't readable by the
`propfirm` user. Check:

```bash
ls -l /etc/propfirm/tls/cert.pem
# Should be readable by the propfirm user (uid 1000)
```

### "invalid PEM file"

The cert is not in PEM format. Check:

```bash
head -1 /etc/propfirm/tls/cert.pem
# Should be: -----BEGIN CERTIFICATE-----
```

If it's `BEGIN TRUSTED CERTIFICATE` (OpenSSL trust format), convert:

```bash
openssl x509 -in trusted.pem -out cert.pem -outform PEM
```

### "private key doesn't match cert"

The key and cert don't pair. Verify with `openssl`:

```bash
# Compare modulus hashes:
openssl x509 -in cert.pem -pubkey -noout | openssl md5
openssl pkey -in key.pem -pubout 2>/dev/null | openssl md5
# Both hashes must match.
```

## Why rustls (not native-tls / openssl)

- **No C dependencies**: pure Rust, smaller attack surface.
- **Memory safety**: rustls has never had a CVE for memory corruption.
- **Performance**: comparable to native-tls in benchmarks; sometimes
  faster due to ring's AVX2 assembly.
- **Default-deny ciphers**: rustls drops TLS 1.0/1.1 and weak ciphers
  without configuration.
