# Changelog

## 1.2.0-alpha.2 - 2026-05-30

- Extended the OpenSSL TLS fallback to Linux and other platforms, not only Windows.
- Improved compatibility with pools that close rustls handshakes but accept XMRig/OpenSSL TLS clients.
- Kept default IP pool TLS behavior aligned with XMRig by only sending SNI when explicitly configured.

## 1.2.0-alpha.1 - 2026-05-30

- Added opt-in TLS 1.2 compatibility for older pools and TLS terminators.
- Added SHA-256 TLS certificate fingerprint pinning plus a runtime warning when TLS is unpinned.
- Added no-auth SOCKS5 proxy support and optional miner signature login parameter support.
- Re-enabled RandomX pipeline batch hashing and moved full dataset initialization to Rayon.
- Kept RandomX algorithm validation strict to avoid silently mining unsupported variants with the wrong configuration.
- Updated troubleshooting and performance guidance for alpha validation.

## 1.1.0 - 2026-05-28

- Split the application entrypoint into `app` modules for logging, daemon mode, and benchmark startup.
- Moved the direct RandomX FFI wrapper under `src/randomx/` to reduce top-level source clutter.
- Temporarily disabled the risky RandomX pipeline fast path and routed batch hashing through the single-hash API for correctness-first validation.
- Added CPU topology based thread planning with optional E-core usage and benchmark thread sweeps.
- Added flushing after stratum submit writes to reduce delayed share submission.
- Added CNB release build workflow and ignored local MCP/tooling logs.
- Added XMRig-style config compatibility, TLS SNI override, and OpenSSL TLS fallback for private proxy compatibility.
- Fixed xmrig-proxy/NiceHash nonce handling by preserving proxy-reserved nonce bytes.
- Tightened mining console output and made noisy TLS close errors easier to read.

## 0.1.1 - 2026-05-23

- Changed log timestamps to local `yy/mm/dd HH:MM:SS` format and tightened console output.
- Fixed c3pool share submission nonce encoding to match the little-endian nonce written into the job blob.
- Fixed repeated nonce scanning by keeping each worker's nonce cursor across mining batches.
- Switched RandomX from default flags to CPU-recommended flags for AES/JIT/Argon2 acceleration.
- Added shared full-memory RandomX dataset initialization with light-mode fallback.
- Delayed hashrate output until the miner is logged in and has produced hashes, avoiding misleading `0 H/s` during dataset initialization.
- Added c3pool proxy logging tooling for comparing XMRig login, job, and submit traffic.
- Bumped package version to `0.1.1`.
