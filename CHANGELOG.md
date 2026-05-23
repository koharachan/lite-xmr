# Changelog

## 0.1.1 - 2026-05-23

- Changed log timestamps to local `yy/mm/dd HH:MM:SS` format and tightened console output.
- Fixed c3pool share submission nonce encoding to match the little-endian nonce written into the job blob.
- Fixed repeated nonce scanning by keeping each worker's nonce cursor across mining batches.
- Switched RandomX from default flags to CPU-recommended flags for AES/JIT/Argon2 acceleration.
- Added shared full-memory RandomX dataset initialization with light-mode fallback.
- Delayed hashrate output until the miner is logged in and has produced hashes, avoiding misleading `0 H/s` during dataset initialization.
- Added c3pool proxy logging tooling for comparing XMRig login, job, and submit traffic.
- Bumped package version to `0.1.1`.
