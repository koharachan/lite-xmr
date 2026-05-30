# lite-xmr v1.2.0-alpha.2

Alpha hotfix for pools that close rustls TLS handshakes but accept XMRig/OpenSSL clients.

## Highlights

- Extended OpenSSL TLS fallback to Linux and other non-Windows builds.
- Preserved XMRig-style TLS behavior for IP pool endpoints by only sending SNI when configured with `--sni`.
- Keeps the alpha.1 TLS features: `--tls-allow-12`, `--tls-fingerprint`, SOCKS5 proxy support, and miner signature login params.

## Validation

- `cargo test --release`: 15 passed.
- `cargo build --release`: passed on Windows x86_64 with VS developer environment.

## Assets

- lite-xmr-v1.2.0-alpha.2-windows-x86_64.zip
- lite-xmr-v1.2.0-alpha.2-windows-x86_64.zip.sha256
