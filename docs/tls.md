# TLS Termination

The propfirm-engine supports **in-process TLS termination** (and
optional **mTLS**) via `rustls`. This is appropriate when:

- You want end-to-end encryption on the private compose network
  (defense in depth against network misconfiguration).
- You're running mTLS to the platform bridge (each service has its
  own cert).
- You have no external TLS-terminating proxy (envoy / nginx / AWS ALB).

When TLS is **disabled** (default), the server runs plain HTTP —
appropriate when behind an external proxy.

mTLS **is wired** in v0.2.0. When `server.tls.client_ca_path` is set,
the server builds a `rustls::server::ServerConfig` with
`WebPkiClientVerifier` and rejects any client that does not present a
certificate signed by that CA.

---

## Configuration

```toml
[server.tls]
enabled          = true
cert_path        = "/etc/propfirm/tls/cert.pem"
key_path         = "/etc/propfirm/tls/key.pem"
client_ca_path   = "/etc/propfirm/tls/ca.pem"   # mTLS; omit / leave null for one-way TLS
```

Env-var equivalents (double-underscore = nested key):

```bash
PROPFIRM_SERVER__TLS__ENABLED=true
PROPFIRM_SERVER__TLS__CERT_PATH=/etc/propfirm/tls/cert.pem
PROPFIRM_SERVER__TLS__KEY_PATH=/etc/propfirm/tls/key.pem
PROPFIRM_SERVER__TLS__CLIENT_CA_PATH=/etc/propfirm/tls/ca.pem   # mTLS
```

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Whether TLS is enabled. |
| `cert_path` | path | `/etc/propfirm/tls/cert.pem` | Path to PEM-encoded leaf cert (+ intermediates). |
| `key_path` | path | `/etc/propfirm/tls/key.pem` | Path to PEM-encoded private key (PKCS#8 or PKCS#1). |
| `client_ca_path` | `Option<PathBuf>` | `None` | When set, enables mTLS — clients must present a cert signed by this CA. |

---

## Certificate formats

- **Cert**: PEM-encoded, leaf first, then intermediates (if any).
- **Key**: PEM-encoded, PKCS#8 or PKCS#1 (RSA). ECDSA keys are also supported.
- **Client CA** (mTLS only): PEM-encoded, one or more trusted CA certs.

The server uses `axum-server` with the `tls-rustls` feature. `rustls-pemfile`
parses the PEM input. The cert chain and private key are loaded into a
`rustls::server::ServerConfig` (built manually for mTLS, or via
`RustlsConfig::from_pem` for one-way TLS).

---

## How mTLS is wired (the actual implementation)

`src/api/server.rs::load_tls_config()` reads the `TlsSettings` and branches:

### One-way TLS (default path)

When `client_ca_path` is `None`, the server uses the simple `from_pem`
constructor — axum-server handles cert + key parsing internally:

```rust
let config = axum_server::tls_rustls::RustlsConfig::from_pem(cert, key).await?;
```

### mTLS (when `client_ca_path` is `Some`)

When `client_ca_path` is set, the server builds the `rustls::ServerConfig`
directly so it can install the `WebPkiClientVerifier`:

```rust
use rustls::server::WebPkiClientVerifier;

// 1. Load the server cert chain + private key.
let certs: Vec<rustls::pki_types::CertificateDer<'static>> = {
    let mut reader = std::io::BufReader::new(cert.as_slice());
    rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()?
};
let private_key = rustls_pemfile::private_key(&mut std::io::BufReader::new(key.as_slice()))?
    .ok_or_else(|| anyhow::anyhow!("no private key found"))?;

// 2. Build the root cert store from the configured client CA.
let ca_pem = std::fs::read(client_ca_path)?;
let mut root_store = rustls::RootCertStore::empty();
for ca_cert in rustls_pemfile::certs(&mut std::io::BufReader::new(ca_pem.as_slice()))
    .collect::<Result<Vec<_>, _>>()?
{
    root_store.add(ca_cert)?;
}

// 3. Install the WebPkiClientVerifier — clients without a valid cert
//    signed by this CA are rejected at handshake time.
let verifier = rustls::server::WebPkiClientVerifier::builder(root_store.into())
    .build()
    .map_err(|e| anyhow::anyhow!("failed to build mTLS verifier: {e}"))?;

let server_config = rustls::server::ServerConfig::builder()
    .with_client_cert_verifier(verifier)              // <- the mTLS hook
    .with_single_cert(certs, private_key)?;

let config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(server_config));
```

On startup the server logs:

```
INFO propfirm::api::server: mTLS client verification enabled client_ca=/etc/propfirm/tls/ca.pem
```

### Why `WebPkiClientVerifier` (not `AllowAnyAuthenticatedClients`)

The legacy `AllowAnyAuthenticatedClients` verifier from rustls 0.22 and
earlier is deprecated in rustls 0.23 (the version this engine pins). The
`WebPkiClientVerifier` is the recommended successor — it uses the `webpki`
crate for path validation and supports revocation (CRL) lists, custom
signature policies, and edge-cases like cross-signed CAs.

For the typical "trust one CA, accept any cert signed by it" pattern,
the code above (`WebPkiClientVerifier::builder(root_store).build()`) is
the equivalent of the old `AllowAnyAuthenticatedClients::new(root_store)`.

---

## Generating a self-signed cert + CA (dev / testing)

For one-way TLS:

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

For mTLS (generate a CA, a server cert signed by it, and a client cert
signed by it):

```bash
# 1. CA key + cert
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout /tmp/ca.key -out /tmp/ca.pem -days 3650 \
  -subj '/CN=propfirm-dev-ca'

# 2. Server key + CSR → sign with CA
openssl req -newkey rsa:2048 -nodes \
  -keyout /tmp/server.key -out /tmp/server.csr \
  -subj '/CN=localhost' \
  -addext 'subjectAltName=DNS:localhost,IP:127.0.0.1'
openssl x509 -req -in /tmp/server.csr -CA /tmp/ca.pem -CAkey /tmp/ca.key \
  -CAcreateserial -out /tmp/cert.pem -days 365 -sha256

# 3. Client key + CSR → sign with CA
openssl req -newkey rsa:2048 -nodes \
  -keyout /tmp/client.key -out /tmp/client.csr \
  -subj '/CN=platform-bridge'
openssl x509 -req -in /tmp/client.csr -CA /tmp/ca.pem -CAkey /tmp/ca.key \
  -CAcreateserial -out /tmp/client.pem -days 365 -sha256

# 4. Run the server with mTLS:
PROPFIRM_SERVER__TLS__ENABLED=true \
PROPFIRM_SERVER__TLS__CERT_PATH=/tmp/cert.pem \
PROPFIRM_SERVER__TLS__KEY_PATH=/tmp/server.key \
PROPFIRM_SERVER__TLS__CLIENT_CA_PATH=/tmp/ca.pem \
cargo run --release --features server --bin propfirm-server

# 5. Verify — without a client cert, the handshake fails:
curl --cacert /tmp/ca.pem https://localhost:8080/health
# Expected: connection reset / handshake failure

# 6. Verify — with a client cert, it succeeds:
curl --cacert /tmp/ca.pem \
  --cert /tmp/client.pem --key /tmp/client.key \
  https://localhost:8080/health
# Expected: ok
```

---

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

For mTLS in production, the operator typically maintains an internal CA
(vault / cert-manager's `clusterissuer` of kind `SelfSigned` or a private
PKI). The platform bridge is issued a client cert from that CA; the engine
configures `client_ca_path` to the CA bundle.

---

## Recommended deployment topologies

### Topology A — in-process TLS, no external proxy

```
[ platform bridge ] --TLS--> [ propfirm-server:8080 ]
```

Use when:
- Private Network only
- Single ingress point
- You want defense-in-depth against misconfigured network policies

Config: `tls.enabled = true`, `tls.client_ca_path` unset (one-way TLS).

### Topology B — plain HTTP behind external proxy

```
[ platform bridge ] --TLS--> [ envoy/nginx/k8s-ingress ] --HTTP--> [ propfirm-server:8080 ]
```

Use when:
- You already operate a TLS-terminating gateway
- You need cert-manager / Let's Encrypt automation
- Multiple services behind the same gateway

Config: `tls.enabled = false`.

### Topology C — mTLS to platform bridge

```
[ platform bridge (with client cert) ] --mTLS--> [ propfirm-server:8080 ]
```

Use when:
- You need cryptographic client identity verification
- Defense in depth against compromised network credentials
- The platform bridge is the sole caller (matches the engine's
  no-auth, internal-only model)

Config: `tls.enabled = true` + `tls.client_ca_path = /path/to/ca.pem`.

This is the recommended production topology for the propfirm-engine.
The engine has no in-process authentication (intentionally removed in
v0.2.0); mTLS at the network boundary is the equivalent cryptographic
identity check.

---

## Verification

After enabling TLS:

1. Check the server logs show `tls_enabled=true`:

   ```bash
   docker compose logs propfirm-server | grep tls_enabled
   ```

2. For one-way TLS, `curl --insecure https://localhost:8080/health` returns `ok`.

3. For mTLS, verify a missing client cert is rejected:

   ```bash
   # Without --cert --key, the handshake fails:
   curl --cacert /etc/propfirm/tls/ca.pem https://localhost:8080/health
   # Expected: handshake failure / connection reset

   # With --cert --key, succeeds:
   curl --cacert /etc/propfirm/tls/ca.pem \
     --cert /etc/propfirm/tls/client.pem \
     --key  /etc/propfirm/tls/client.key \
     https://localhost:8080/health
   # Expected: ok
   ```

4. For Let's Encrypt certs:

   ```bash
   curl https://propfirm.internal.example.com/health
   # Expected: ok (no --insecure needed)
   ```

---

## Cipher suite selection

The `rustls` default cipher suite list is curated by the Rust
security team and is the recommended set. We do NOT override it.

If your compliance regime requires a specific cipher list, you'll
need to fork the `rustls::server::ServerConfig` builder — out of scope
for the default topology.

---

## Common issues

### "TLS cert read failed"

The path in `cert_path` doesn't exist or isn't readable by the
`propfirm` user. Check:

```bash
ls -l /etc/propfirm/tls/cert.pem
# Should be readable by the propfirm user (uid 1000)
```

### "mTLS client CA read failed"

The path in `client_ca_path` doesn't exist or isn't readable. Same
check as above for `ca.pem`.

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

### "failed to build mTLS verifier"

The `WebPkiClientVerifier::build()` call returned an error. Common
causes:
- The CA bundle is empty (no `BEGIN CERTIFICATE` lines parsed).
- The CA bundle contains unsupported signature algorithms (rustls'
  default ring-based crypto provider supports RSA-PSS, ECDSA P-256 /
  P-384, Ed25519 — not RSA-PKCS1-v1.5 or older curves).

Fix by regenerating the CA with a supported algorithm.

### "connection reset by peer" (mTLS, no client cert)

This is the **expected** behaviour — the server rejected the
handshake because the client didn't present a cert. Provide a client
cert signed by the configured CA.

---

## Why rustls (not native-tls / openssl)

- **No C dependencies**: pure Rust, smaller attack surface.
- **Memory safety**: rustls has never had a CVE for memory corruption.
- **Performance**: comparable to native-tls in benchmarks; sometimes
  faster due to ring's AVX2 assembly.
- **Default-deny ciphers**: rustls drops TLS 1.0/1.1 and weak ciphers
  without configuration.
- **First-class mTLS**: `WebPkiClientVerifier` is the modern,
  audited-by-default verifier; native-tls's mTLS surface is OpenSSL /
  platform-specific and inconsistent across platforms.
