# lite-xmr v1.2.0-alpha.1

Alpha release for TLS compatibility, safer TLS pinning, proxy connectivity, and RandomX performance validation.

## Highlights

- Added `--tls-allow-12` for TLS 1.2-only pools and legacy TLS terminators.
- Added `--tls-fingerprint <SHA256>` certificate pinning and a warning for unpinned TLS sessions.
- Added no-auth SOCKS5 proxy support with `--socks5 <HOST:PORT>`.
- Added `--miner-signature <SIG>` to include `sig` in Stratum login params.
- Re-enabled RandomX pipeline batch hashing and switched full dataset initialization to Rayon.
- Kept unsupported RandomX variant jobs rejected to avoid mining with the wrong algorithm configuration.

## Validation

- `cargo test --release`: 15 passed.
- `cargo build --release`: passed on Windows x86_64 with VS developer environment.

## Assets

- lite-xmr-v1.2.0-alpha.1-windows-x86_64.zip
- lite-xmr-v1.2.0-alpha.1-windows-x86_64.zip.sha256
