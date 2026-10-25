# iLO 3 crypto subsystem — software, no hardware accelerator

Date: 2026-10-21. Static RE of `os_main_base0.bin` + boot ROM. Answers "is there a crypto
engine to write a driver for?" — **No. iLO 3 crypto is entirely software.** Relevant to the
Rust-rewrite plan: there is **no HW crypto to offload to**, so TLS is software-bound on the
250 MHz ARMv5 (why iLO 3 HTTPS is slow).

## Evidence — all software

| Primitive | Implementation | Evidence |
|-----------|----------------|----------|
| **AES** | **software table-based Rijndael** | T-tables present: Te0 `0xC66363A5` @ `0x291994`, Td0 `0x51F4A750` @ `0x290494`. `dvcaes` (KVM stream encrypt). HW AES would not ship T-tables. |
| **RC4** | software | `dvcrc4sw` (the `sw` is explicit) |
| **SHA-256** | software | boot ROM constant tables (`docs/31`); combined SHA-256/512 K-table |
| **RSA-2048** | software bignum modexp | boot ROM sig-verify (`docs/31`) |
| **crypto library** | **"klite"** (software) | `corsec_klite.c`, `klite_server`, `kliteMem`, `klite_uninit` |
| **TLS / SSL** | **Allegro** toolkit (software) | `corsec_allegro` (the Allegro SSL/web library) |
| **RSA/SSL glue** | software | `corsec_rsasslc` |

## Architecture — client/server, not a driver

The "ASIC" in `corsec_asic` is misleading: `src\corsec_asic.c` is a **socket RPC client**
(`setsockopt`, send/recv data + header errors) that forwards crypto requests to a separate
**`klite_server`** crypto task. No MMIO. So:

```
app  ->  corsec_* client  --(local socket)-->  klite_server task  ->  software crypto (klite)
                                                   (+ Allegro for SSL/TLS)
```

There is **no crypto IODevice in the kernel BSP** and **no crypto MMIO register block**.

## Correction to docs/05

docs/05 listed a "crypto engine ctrl/status" at **`0xC000000C`**. This is **wrong**: `0xC000000C`
has **zero literal references and zero `[base,#0xC]` accesses** in both the boot ROM and the OS,
and the sig-verify path (which docs/05 claimed touched it) does not. There is no crypto register
there — the entry was speculative. Removed/flagged in docs/05.

## RNG / entropy — software PRNG, no hardware TRNG

- The RNG is a **software PRNG** inside the klite crypto lib (`PRNG`/`RNG` strings in the klite
  region `0x30d0xx`). No **RNG IODevice** in the kernel BSP; no RNG MMIO register found.
- `entropy` appears only as a **crypto parameter name** (in a field list with `keySize`/`keyBytes`/
  `round`/`input`/`output`), not a hardware noise source.
- `rand_Turk` was a **false hit** — the `America/Grand_Turk` timezone string.

→ **No hardware entropy source.** The original seeds a software PRNG from software state — a
classic weak spot for an embedded TLS stack. The Rust rewrite must build its own entropy pool.

## SSL / TLS stack + key/cert storage

- SSL API is **OpenSSL-compatible** (Allegro / OpenSSL-derived): `SSL_CTX_NEW`,
  `SSL_CTX_CHECK_PRIVATE_KEY`, `SSL_CREATE_CIPHER_LIST`, `SSL_BYTES_TO_CIPHER_LIST`,
  `SSL_CERT_FILE`/`SSL_CERT_DIR`, `SSL_CTX_LOAD_VERIFY_LOCATIONS`.
- Key/cert material: `CERT_STORE`, `SERVER_CERT`, `PRIVATE_KEY`, formats **PKCS1 / PKCS7 /
  PKCS12** (PEM/DER). Served on `HTTPS_PORT`. Stored in NVRAM/flash (the iLO self-signed or
  user-imported TLS cert + key).

## Implications for the Rust rewrite

- **No crypto driver to write** — nothing to accelerate; skip it.
- **TLS = pure software** on 250 MHz ARMv5. Use **`rustls`** (the modern replacement for iLO 3's
  Allegro/klite). Expect **slow handshakes** — same inherent limit as the original firmware.
- Mitigations (software-only): prefer **ECDHE + AES-GCM/ChaCha20** over RSA key exchange;
  **TLS session resumption / tickets** to avoid repeat handshakes; keep the web surface minimal;
  consider **ChaCha20-Poly1305** (faster than AES in software on a core with no AES instructions).
- Architecture to mirror: a dedicated **crypto/TLS task** (like `klite_server`) so crypto work
  doesn't stall the management loop — in Rust, an async task owning `rustls`.

## Open

- ~~Hardware TRNG?~~ **RESOLVED: none** — RNG is a software PRNG (klite); no RNG MMIO / IODevice.
  The Rust rewrite MUST build a software entropy pool (timer jitter via `0xC0000098` counter LSBs,
  interrupt/IO timing, MAC RX jitter). Treat key-quality entropy as a first-class task, not an
  afterthought — the original's software PRNG was a likely weakness.
- The exact `klite` crypto API (request/response framing over the socket) if a drop-in service
  boundary is wanted; for a clean Rust rewrite, just use `rustls` directly instead.

## Rust rewrite: entropy pool implemented (2026-10-21)

Built `firmware/src/rng.rs` + `firmware/src/chacha.rs` — the first-class software entropy pool
docs/32 calls for. Design: a **sponge pool** (ChaCha20 permutation over a 16-word state) absorbs
noise samples and seeds a **reseedable ChaCha20 DRBG**; exposed as `rand_core::RngCore +
CryptoRng` (`SystemRng`) for the rand/rustls ecosystem. Self-contained ChaCha20 (no ppv-lite86/
SIMD deps that fight ARMv5).

**Entropy sources mixed in** (all low-credit, conditioned by the pool — none trusted alone):
1. **Timer jitter** (primary): counter `0xC0000098` LSBs sampled around a small variable-timing
   memory workload, 4096 samples at boot (`rng::init`). Leans on IRQ preemption + DRAM-refresh
   stalls (the ~1 MHz placeholder counter is too coarse for per-instruction jitter).
2. **IRQ-arrival timing** (continuous): the Wallace tick handler folds the counter value at each
   interrupt into the pool (`rng::feed_timing`).
3. **Tach + temperature + read-timing** (continuous): the thermal task stirs in one fan's raw tach
   count, the per-sensor temperature demands, and the counter at sample time each tick. Sensor/fan
   LSB + edge-phase noise — low-rate, correlated, so **near-zero credited entropy**, but free and
   strictly additive.

**MUST DO before trusting for keys** (CONFIRM-ON-HW): a NIST SP 800-90B-style entropy assessment
on real hardware with the real counter rate. Until then the pool output is UNVERIFIED — do not
generate TLS/host keys from it. (iLO 3's original software PRNG was a likely weakness; getting this
right is the point.)
