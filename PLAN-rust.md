# PLAN-rust.md — clean-room iLO 3 firmware rewrite in Rust

Goal: a lean, modern, **from-scratch Rust BMC firmware** for the Gromit-XE (iLO 3) — replacing
the EOL HP firmware with something that fits the hardware (250 MHz ARMv5, ~60 MB usable RAM, 8 MB
flash) and has current crypto/TLS. Chosen over OpenBMC because OpenBMC's Linux+phosphor+bmcweb
footprint does not fit (see the resource analysis in the chat / PLAN.md).

## Clean-room method (keep it legal)

- **Hardware register maps are facts, not copyrightable** — `docs/02`–`32` (register addresses,
  bit-fields, init sequences) are the legitimate interop foundation; use them directly.
- **Standard protocols** (SSH, SNMP, HTTP/TLS, Redfish, IPMI, SNTP, syslog, DHCP) are **RFC/public
  specs** — reimplement from the standards + existing Rust crates, NOT from HP's code. RE the HP
  binary only to learn *which* features/OIDs/endpoints exist, then build to spec.
- **HP-proprietary bits** (RIBCL XML schema, CHIF framing, DVC video) must be RE'd from the binary
  (behaviour/wire-format), then reimplemented clean.
- Do not copy HP code or redistribute HP binaries (repo rule). This is interop/repair on owned HW.

## Architecture (target)

- **`no_std` Rust** firmware, boots from flash (reuse DDR/clock init `docs/30`, reset `docs/31`).
- **`embassy`** async executor (DECIDED 2026-10-21, see docs/47 — iLO 3 has no hard-RT
  requirement; event-driven soft-RT only, so no preemptive RTOS. Reversible "for now"; upgrade
  ladder if slow-crypto fairness bites = **RTIC 2** (pure-Rust preemptive, write an ARMv5 backend)
  → **FreeRTOS-rust**/ARM926 (C kernel, last resort)). HAL modules = Rust ports of
  the 9 drivers already written. Safety (watchdog/thermal fail-safe) = timer ISR above the executor.
- **`smoltcp`** TCP/IP; **`rustls`** TLS (replaces Allegro/klite — `docs/32`).
- Workspace layout (proposed): `rebuild/ilo3-rs/` with crates `hal/`, `net/`, `crypto/`,
  `services/` (web, ssh, snmp, ipmi), `boot/` (SPL), `bin/` (the firmware image).

## RE status of the hardware layer (done — see docs)

☑ CPU/SoC, boot ROM, DDR/clock, reset/secure-boot ☑ UART/timer/irqchip/I2C/SPI/GPIO/PECI/fan/MAC
☑ CHIF host interface ☑ KVM video ☑ sensors/power/PSU/inventory data paths ☑ **crypto = software,
no HW engine/TRNG (`docs/32`)**. The 9 Linux drivers are the HAL reference to port to Rust.

## Application-layer RE still needed (for the rewrite)

Each entry: what to RE from the binary vs what is a standard to build to spec.

| Subsystem | RE from binary | Build to spec (crate) | Doc |
|-----------|----------------|-----------------------|-----|
| ~~**Web server / HTTPS**~~ ☑ | RomPager+Allegro SSL; **58 `json/*` endpoints**; dispatch_json; cert/CSR store | HTTP + `rustls` + minimal SPA | **docs/33 done** |
| ~~**SSH / CLI**~~ ☑ | RomSShell 4.62; SMASH CLP + HP verbs (power/vm/vsp/textcons); /map1 + /system1 tree | `russh` (modern only) + SMASH CLP engine | **docs/34 done** |
| ~~**SNMP**~~ ☑ | v1/v2c (no v3); CPQ MIBs (CPQRIB3/CPQCIDRV/cpqHlth); power/host/health traps ×3 dest; +email(SMTP) | Rust agent (+add v3); public CPQ MIBs → our data | **docs/35 done** |
| ~~**IPMI**~~ ☑ | IPMI 2.0 + DCMI 1.5 + HP OEM (OEMHP_POWER/RC/VM); KCS/LAN/CHIF | IPMI responder to spec | **docs/36 done** |
| ~~**RIBCL / XML scripting**~~ ☑ | XML envelope; 7 info blocks; ~180 cmds (GET_EMBEDDED_HEALTH, SET_HOST_POWER, MOD_*) | quick-xml handler; Redfish primary + minimal RIBCL | **docs/37 done** |
| **Redfish** | n/a (iLO 3 predates Redfish) | optional: a minimal Redfish subset = modern alt to RIBCL | — |
| ~~**Auth / users / sessions**~~ ☑ | 12 users; bcrypt; privilege bitmap (login/RC/vmedia/power/config/admin+ext) | argon2 + bitflags; enforce everywhere | **docs/38 done** |
| **Network services** | DHCP/DNS/SNTP/syslog client config in NVRAM | all RFC standards: `smoltcp` DHCP, SNTP, syslog, mDNS | — |
| ~~**Event / IML log**~~ ☑ | IML(SystemRecord)+iLO log(MapRecord): coalescing severity records; AHS telemetry | circular wear-leveled store + EventBus | **docs/39 done** |
| **LDAP / directory auth** | the directory integration config | LDAP client to spec | — |
| ~~**NVRAM / config store**~~ ☑ | named K-V store (NvramDisk); CFG_READ/WRITE by name; per-key priv/size | wear-leveled Rust K-V store | **docs/38 done** |

## Build phases

1. **Bring-up** — `no_std` Rust boots from flash: DDR/clock (`docs/30`), UART console, timer,
   irqchip. Port the 4 core drivers (UART/timer/irqchip + GPIO) to Rust HAL.
   - ⏳ **IN PROGRESS** — workspace scaffolded at `rebuild/ilo3-rs/` (see its README).
     ☑ builds/links for `armv5te-none-eabi` (entry `0x20000000`, flat .bin ~10 KB);
     ☑ reset/startup (CP15, IRQ+SVC stacks, BSS zero); ☑ polled console UART (`0xC00000F0`).
     ☑ `critical-section` impl (CPSR IRQ-mask, no LDREX — `src/critsec.rs`) ·
     ☑ timer (64-bit monotonic `0xC0000098` + `delay_us`; Wallace clockevent `0xD1000000`/IRQ36
     register-exact — `src/timer.rs`) · ☑ irqchip (2-bank, 64 IRQs — `src/intc.rs`) ·
     ☑ ARM exception vectors + root IRQ dispatch (disasm-verified — `src/vectors.rs`).
     ☑ embassy executor on ARMv5 (`embassy-executor` 0.10 `platform-spin`; 2 async tasks
     interleave via `yield_now`). Needed a custom target (`armv5te-ilo3.json`, `atomic-cas`)
     + `__atomic_*` single-core libcall shims (`src/atomics.rs`) — ARM926 has no LDREX.
     ☑ MMU bring-up (`src/mmu.rs`): ARMv5 one-level (1 MB section) table — identity-map all
     4 GB (docs/10 VA=PA), MMIO device / DDR write-back cached, `L1[0]`=fault (null-catch);
     **high vectors** (SCTLR V=1 → base `0xFFFF0000`) with section `0xFFF00000` aliased to a
     reserved top DDR MB (`_vectors_phys` `0x20F00000`, link.x) so the vector table is writable.
     CP15 enable seq (TTBR0/DACR-manager/inval-TLB+cache/drain-WB/SCTLR M·C·I·V/prefetch-flush)
     disasm-verified. **Live IRQ now wired**: vectors installed @ base, Wallace tick (IRQ 36)
     → counting handler, CPSR I-bit cleared; heartbeat prints `irq_ticks`. (Wallace reload rate
     + tick ack = CONFIRM ON HW.)
     ☑ `embassy-time-driver` (`src/time_driver.rs`): global `Driver` over the 64-bit counter
     (`now()`, 1 MHz = embassy `tick-hz-1_000_000`) + a heapless generic timer queue (cap 16,
     standalone so it doesn't pin the executor version). Tick-based wake: `schedule_wake` parks
     the waker, the live Wallace IRQ calls `on_tick` → `next_expiration` wakes elapsed deadlines.
     `heartbeat`/`ticker` now sleep on real `Timer::after` (1 s / 500 ms), not busy-wait. (Timer
     resolution = Wallace period; one-shot reload is the HW-confirmed refinement.)
     ☑ SPL / first-stage loader (`spl/` crate, XIP @ `0xFFFF0000` = flash `0x7f0000`, the HP
     boot-ROM replacement of docs/31): reset vectors + CP15 (MMU/caches off) + scratch SP
     `0x20004000` → **DDR3 init register-exact from docs/30** (`spl/src/ddr.rs`: warm-check,
     500-tick delay, phase-1 timing table, `0x3` start, bit7 cal-poll, phase-2 `0x2`+`0x80200040`,
     MEMCFG=strap>>1) → raise SP into high DRAM → **copy main fw flash→DDR → jump `0x20000000`**
     (`spl/src/flash.rs`; flash is XIP-readable, so load = memcpy, no SMIF FIFO). Packaging:
     `tools/pack-flash.py` builds the 8 MB image (payload+header @0, SPL @`0x7f0000`); byte-verified.
     SPL flat bin = 1.4 KB. **Clock gap RESOLVED** (deeper boot-ROM RE, docs/30 2026-10-21 update):
     the operative path (fn `0x7f198c`) does NO PLL/clock reprogram — DDR trains on the strap-default
     clock; the PLL (`0x80200000+0x88`) is touched only on the `*0xD1000000 & 3 == 0` branch, which
     the fitted board doesn't take (real DDR values are in the static table). `ddr.rs` is a verbatim
     port of the `0x7f1a0c` table (ROM order, byte-verified) + `clock_mode()` self-check (warns if 0)
     + bounded cal-poll (Status::CalTimeout → halt, no infinite spin). HW-confirms: ASIC rev >4 and
     `*0xD1000000 & 3 != 0` (SPL self-checks the latter); full path = CONFIRM-ON-HW via SPI programmer.
     ☐ hardware validation (board / custom QEMU machine).
2. **Platform HAL** — ⏳ **IN PROGRESS**. ☑ All 6 register drivers ported to the Rust HAL
   (`firmware/src/`, build-clean, dead-code-eliminated until services consume them):
   `cpld.rs` (bit-addressable CPLD/pwrmicro: UID/PSU/fan-latch, docs/08-09) ·
   `spinor.rs` (SMIF flash controller read/erase/program, docs/15/17) ·
   `i2c.rs` (8-engine I2C/SMBus, docs/16) · `peci.rs` (native PECI engine + rev-strap timing,
   docs/26; descriptor/DMA = CONFIRM-ON-HW) · `fan.rs` (CPLD PWM/tach scan-chain + latch,
   docs/27) · `mac.rs` (2 NICs: MDIO/reset/station-addr/mcast-hash CONFIRMED; TX/RX DMA rings
   deferred to Phase 3, docs/11). ⏳ **Services** on the HAL:
   ☑ `power.rs` (host power HAL — state read + on/graceful/force/warm-reset/NMI/cold-cycle,
   docs/24, all CONFIRMED) · ☑ `sensors.rs` (temp read mechanisms — LM75/I2C + PECI GetTemp;
   topology/thresholds = SDR placeholder, docs/25) · ☑ **`thermal.rs`** — the SAFETY-CRITICAL
   fan service (docs/42): per-sensor demand → FASTEST/SINGLE/AVERAGE grouping → clamp+hysteresis
   → scan-chain PWM, with the **fail-safe** (boot=MAX, any sensor error→zone MAX, critical→graceful
   host shutdown). Live embassy task; first service that links the whole HAL chain (bin +3.5 KB).
   Fan-control is iLO's small role (BIOS owns policy) — curve/thresholds/groupings = board SDR
   (PLACEHOLDER, validate under thermal load before trusting).
   ☑ `psu.rs` (panda PSU telemetry over I2C, docs/19/20: `[reg,cksum]`→`[lo,hi,pec]` transaction
   with sum-to-0 check + 3 retries, fw-rev/status/current/peak; presence/enable via CPLD; raw→
   physical scale = SDR Open) · ☑ `drives.rs` (backplane SEP over I2C, docs/23: ACK-probe discovery
   0x70-0x77, UID-set D8/status D2/SGPIO D2 frames — CONFIRMED frame literals; D8 chk + bit masks
   flagged). Both build-clean, DCE'd until a health/inventory/SNMP consumer wires them.
   ☐ SMBIOS CPU/mem inventory (docs/22) — **blocked on CHIF** (ROM-CHIF transport), deferred to
   Phase 6. ☐ HW bring-up of the flagged items. **Phase 2 is otherwise complete** (6 drivers +
   power/sensors/thermal/psu/drives; only the CHIF-dependent SMBIOS path remains).
3. **Networking** — ⏳ **IN PROGRESS**. ☑ `smoltcp` 0.12 TCP/IP stack built (`firmware/src/net.rs`):
   `GxeDevice` phy over a `MacRing` seam + an embassy `net_task` running the interface and a
   **DHCPv4** client (applies address/router on `Configured`). Builds clean, live task (bin +28 KB).
   **BLOCKED at the datapath**: the MAC TX/RX DMA ring is RE-confirmed *not* statically recoverable
   (docs/11 2026-10-21 update — the driver drives INTEGRITY DMA object methods, no descriptor-base
   MMIO), so `StubRing` yields no RX / drops TX. The entire stack is ready; reconstruct the ring on
   hardware (observe descriptor memory + MAC regs on a live board) and implement `MacRing` for it.
   ☐ DNS/SNTP/syslog (RFC crates) on top once the ring passes traffic. MDIO + MAC config usable now.
4. **Crypto/TLS** — ⏳ **IN PROGRESS**. ☑ **software entropy pool** (`firmware/src/rng.rs` +
   `chacha.rs`, docs/32): sponge pool (ChaCha20 permute) → reseedable ChaCha20 DRBG, `rand_core`
   `RngCore`+`CryptoRng` (`SystemRng`). Sources: boot timer-jitter harvest (4096 samples) + Wallace
   IRQ-arrival timing + thermal-task tach/temp/read-timing stirring — all low-credit, conditioned.
   Live at boot; self-contained (no ppv-lite86). **Output UNVERIFIED** — needs a NIST SP 800-90B
   entropy assessment on real HW before any key is generated from it.
   ☑ **TLS 1.3 crypto primitives** (`firmware/src/crypto.rs`): vetted RustCrypto crates that build
   clean no_std on ARMv5 — `chacha20poly1305` (AEAD), `sha2`, `hkdf`/`hmac`, `x25519-dalek` (ECDHE)
   = the `TLS_CHACHA20_POLY1305_SHA256` suite. Keyed from `rng::SystemRng`. **Power-on self-test**
   (`crypto::selftest`): SHA-256 + X25519 KATs (FIPS/RFC vectors) + AEAD round-trip & tamper check,
   run at boot (+24 KB linked). (Can't execute here; vectors are RFC-exact, self-checks on HW.)
   ☑ **heap / global allocator** (`firmware/src/heap.rs`): `embedded-alloc` LLFF over the DDR gap
   between `.bss` and the stack (link.x `_heap_start`.._heap_end` ≈ 14.8 MB), locked via our
   critical-section; NOLOAD (no boot cost). `alloc` is now available firmware-wide.
   ☑ **TLS 1.3 key schedule + record layer** (`firmware/src/tls13.rs`, RFC 8446 §7.1/§5.2):
   Extract / Expand-Label / Derive-Secret → early/handshake/master secret tree → per-direction
   traffic secrets → {key, iv, finished_key}; transcript hash; Finished MAC; record AEAD with
   nonce = iv⊕seq. Built on the primitives + heap; RFC-verifiable crypto core.
   ☑ **cert/key store** (`firmware/src/certstore.rs`): PEM→DER (hand-rolled base64), server cert +
   PKCS8 key held on the heap, persisted in a 64 KB flash block via `spinor` (CONFIRM-ON-HW).
    ☑ **TLS 1.3 handshake state machine** (`firmware/src/tls_handshake.rs`, 2026-10-22) — full server
    side: ClientHello parse, ServerHello, EncryptedExtensions, CertificateRequest, Certificate,
    CertificateVerify (Ed25519, `64 spaces ‖ context ‖ 0x00 ‖ hash`), Finished, mTLS client-cert
    chain verify. Streaming `client_record` for multi-record flights; record padding + inner
    content-type + per-key-epoch seq reset handled. **Validated end-to-end against real OpenSSL
    `s_client`** (TLS 1.3 + CHACHA20 + X25519 + Ed25519 + mTLS + app data; secrets/keys byte-match
    `-keylogfile`). X.509 parsing is hand-rolled (`src/x509.rs`, Ed25519 only) — `x509-parser` won't
    build on no_std (nom→minimal-lexical needs std). Not HW-gated; the stack is ready.
5. **Management services** — ⏳ **IN PROGRESS**. Foundations first:
   ☑ **account DB + privilege model** (`firmware/src/users.rs`, docs/38): ≤12 accounts, Argon2id
   PHC hashes (tuned to 4 MB/3 passes to fit the heap; salt from the entropy pool), `Privileges`
   bitflags (6 core + 7 extended), login lockout. ☑ **NVRAM config K-V store** (`firmware/src/
   nvram.rs`, docs/38): named keys + per-key privilege mask, ping-pong wear-leveled across two
   64 KB flash blocks with a seq number + CRC (atomic: a mid-write power loss falls back to the
   prior block). Both build-clean, flash I/O = CONFIRM-ON-HW.
   ☑ **web auth wired** (`firmware/src/webauth.rs`): session-based login — `POST /json/login` takes
   HTTP Basic creds, runs ONE Argon2 verify (outside any lock — hashing never holds a critical
   section), issues a random 256-bit bearer token (30-min TTL). An `Auth` extractor validates
   `Authorization: Bearer <token>` on every `json/*` endpoint and enforces `Privileges`; `logout`
   invalidates the token. A default `Administrator` (random password, logged once) is provisioned at
   boot if the DB is empty.
   ⏳ **IPMI** (docs/36): ☑ message core + dispatch (`firmware/src/ipmi.rs`, transport-agnostic —
   NetFn/cmd → completion-code+data) with Get Device ID, Get Chassis Status, Chassis Control
   (→ host-power HAL), Get Sensor Reading (→ sensors); ☑ KCS BMC-side state machine
   (`firmware/src/kcs.rs`, IPMI §9) — register offsets at 0x01F0D400 flagged CONFIRM-ON-HW (docs/05
   only locates the block), not spawned yet. ☐ DCMI, SDR/SEL/FRU store, IPMI-over-LAN (RMCP+),
   HP OEM (OEMHP_*).
   ☑ **event log store** (`firmware/src/eventlog.rs`, docs/39): one coalescing record model, two
   views (IML hardware / iLO management+actor), circular ring (CAP 256, oldest drops) with duplicate
   coalescing (bump count+last-seen), ping-pong flash persistence (CRC+seq). `raise()` = the
   EventBus entry point (SNMP-trap/email hooks later); backs web/IPMI-SEL/RIBCL views. Timestamps =
   monotonic counter until NTP (docs/18).
   ⏳ **SNMP** (docs/35): ☑ ASN.1 BER codec (`firmware/src/ber.rs`, the SNMP subset) + ☑ v1/v2c
   agent (`firmware/src/snmp.rs`) — community-checked GET/GETNEXT over a MIB (system group +
   HP/Compaq 1.3.6.1.4.1.232 health OID bound to sensors) + an SNMPv2-Trap builder. Transport-
   agnostic (`handle(packet)→packet`); UDP listener on smoltcp + trap destinations (3, docs/35) +
   event→trap mapping are the remaining wiring. ☐ v3/USM, email (SMTP) alert channel.
   ⏳ **SSH CLI** (docs/34): ☑ SMASH CLP command engine (`firmware/src/clp.rs`) — CIM target tree
   (/map1, /system1, /lanendpt1) with cd/pwd/show/set + HP verbs (power/uid/nmi/version), bound to
   the HAL (power/cpld/sensors) and privilege-gated; transport-agnostic (`execute(line)→String`).
   ☐ `russh` SSH-2 transport (modern KEX/ciphers only — no SSH-1/3DES) on smoltcp, pubkey/password
   auth via the account DB.
   ☑ **RIBCL** (`firmware/src/ribcl.rs`, docs/37): compact XML tokenizer + dispatcher — parses the
   `<RIBCL><LOGIN><X_INFO MODE><CMD/>` envelope, authenticates each request via `webauth::
   authenticate` (shared lock-safe auth, also used by web login), runs a privilege-gated command
   subset (GET_FW_VERSION, GET_EMBEDDED_HEALTH, SET_HOST_POWER, PRESS_PWR_BTN, COLD_BOOT_SERVER,
   CLEAR_IML/CLEAR_EVENTLOG) bound to the HAL/eventlog, and returns `<RESPONSE STATUS=...>`.
   ☐ the full ~180-command set + XML port transport.
   ⏳ **EventBus wiring**: ☑ producers — the live thermal task raises IML events on sensor-fault /
   critical-temp, web login-fail raises an iLO security event, boot raises BOOT_COMPLETE (coalescing
   dedups repeats); ☑ consumer — web `json/server_event_log` + `json/ilo_event_log` render the store
   (VIEW_LOGS-gated). So the event log now populates + is readable at runtime. ☐ SNMP-trap + email
   fan-out from `raise`, more producers (power transitions, health changes).
   ☐ full json/* set (docs/33) + config (nvram) wiring.
6. **Host interface** — ⏳ **IN PROGRESS**. ☑ CHIF transport core (`firmware/src/chif.rs`, docs/28):
   BAR/doorbell registers, shared-mem window (0x40000000, identity-mapped), `CHIF`/`CHI5` magic +
   14-byte-checksum header validation, per-message length/channel-id header, CCB ownership
   handshake (bit31), 10 channel descriptors, `poll_rx`/`rx_release`/`send` + host doorbell ring.
   Transport only; CCB convention/widths = CONFIRM-ON-HW. ☐ channel→service routing (IPMI-over-PCI
   → `ipmi`, inventory, power), ☐ SMBIOS inventory over ROM-CHIF (docs/22), ☐ host power/state feed.
   HW-gated (needs host PCI + the kernel-provided window).
7. **(hard, optional)** vUSB/vMedia (`docs/29`) + KVM video (`docs/14`) — likely too heavy on
   250 MHz ARMv5; defer / may stay unsupported.

## Web server (picoserve) — implementation plan

Chosen stack (DECIDED 2026-10-22): **picoserve** HTTP on **smoltcp** + our own TLS 1.3 layer
(`src/tls_handshake.rs`), NOT rustls/embassy-net/edge-http. Rationale: rustls's `ServerConnection`
is `std`-gated (no_std = low-level `unbuffered` API only) and its default crypto providers
(aws-lc-rs/ring) won't build on armv5te; embedded-tls is client-only + AES-GCM-only. picoserve is
crypto-agnostic and runs over any async Read+Write stream, so our validated TLS feeds it plaintext.

Data path: `smoltcp TcpSocket → async embedded-io-async adapter → TlsStream (record framing +
encrypt/decrypt) → picoserve`.

- **Deps**: `picoserve 0.20` (default-features off, no `embassy`/`embassy-net`), `embedded-io 0.7`,
  `embedded-io-async 0.7`, `serde` (derive, no_std). Transitive: futures-util, heapless 0.9,
  thiserror 2 — all no_std-clean.
- **Timer**: implement `picoserve::time::Timer` (one method `delay`) over `embassy_time::Timer`.
- **Modules**: `src/tcp.rs` (smoltcp async bridge: `recv_slice`/`send_slice` + `register_recv_waker`/
  `register_send_waker`), `src/tls_stream.rs` (record framing + handshake driver + async Read/Write),
  `src/web.rs` (Router + `json/*` handlers + connection task).
- **Endpoints (first cut)**: `json/overview`, `json/health_summary`, `json/host_power`, `json/network`,
  `json/sessions` — grow to the 58-name contract in `docs/33`.
- **HW-gated**: real traffic waits on the MAC ring (`docs/11`); built + validated structurally against
  a local host harness (curl), like the TLS layer was against OpenSSL.

## Recommended RE order (application layer)

Do the RE just-in-time, one subsystem per pass, each → a `docs/NN`:

1. ~~Web server / HTTPS~~ ☑ **docs/33** — RomPager+Allegro SSL, 58 json/* endpoints, cert/CSR.
2. **IPMI OEM commands** (docs/36) — transport already mapped (KCS/CHIF); RE the HP OEM command set.
3. ~~SSH CLI~~ ☑ **docs/34** — RomSShell; SMASH CLP + HP verbs; /map1 + /system1 tree.
4. ~~SNMP~~ ☑ **docs/35** — v1/v2c, CPQ MIBs, power/host/health traps + email alerts.
5. ~~NVRAM + users~~ ☑ **docs/38** — K-V config store + 12-user DB, bcrypt, privilege bitmap.
6. ~~RIBCL~~ ☑ **docs/37** — XML envelope, 7 info blocks, ~180 commands (publicly documented).
7. ~~IML/event log~~ ☑ **docs/39** — IML+iLO log coalescing records (sev/count/ts); AHS; one store.

## Key constraints (carry forward)

- **Entropy**: no HW TRNG (`docs/32`) — software entropy pool is a first-class task, not an
  afterthought. Bad randomness = broken TLS keys.
- **TLS perf**: software crypto on 250 MHz ARMv5 is slow — prefer ECDHE + ChaCha20-Poly1305, use
  session resumption, keep the web surface small.
- **Flash 8 MB**: keep the image tiny; `no_std` Rust helps. A 16/32 MB flash swap buys headroom.
- **Cooperative-exec crypto risk** (docs/47): embassy can't slice a running `rustls` primitive;
  a long RSA/ECDHE op (tens–hundreds ms on 250 MHz ARMv5) stalls the executor. Design around it:
  ECDHE+ChaCha20, session resumption, small TLS surface, crypto off the hot path (preemptive
  worker if measured bad). This is the one place the embassy choice is load-bearing.
- **ARMv5 Rust**: tier-3-ish; `core::sync::atomic` needs `critical-section`/`portable-atomic`
  (no native LDREX/STREX pre-ARMv6). Expect this friction early in bring-up.

## RE coverage audit (what's done vs what's left)

**No, the entire binary is NOT reverse-engineered — and it doesn't need to be.** Of the ~12 MB
image: a large fraction is **static web-UI assets** (HTML/JS/CSS, jQuery, language packs) that the
rewrite *replaces* with a new SPA, and another large fraction is **third-party libraries**
(INTEGRITY RTOS, Allegro RomPager/RomSShell/SSL, klite crypto, TCP/IP, XML parser) *replaced* by
Rust crates (embassy/russh/rustls/smoltcp/quick-xml). Only the **iLO application logic** needs RE —
we have its architecture; detail is done just-in-time per feature.

### ☑ RE'd to a reimplementable level
- **Hardware / HAL** — register-exact (docs/02-32): SoC, boot/DDR/clock/reset/secure-boot, all 9
  peripheral drivers, sensors/power/PSU/inventory, CHIF, KVM-video, crypto (=software).
- **Management services (contract level)** — web+JSON API (33), SSH/SMASH-CLP (34), SNMP (35),
  IPMI/DCMI (36), RIBCL (37), NVRAM/users/privileges (38), IML/event logs (39).

### ☐ Not yet RE'd — categorized

**A. Standard protocols** — ☑ **docs/41** (NTP/SNTP, SMTP/AlertMail, syslog, LDAP+Kerberos,
SSO, 2FA/smartcard, WS-Man, mDNS/SLP): presence+config/modes captured; all build-to-spec.

**B. iLO-proprietary logic — genuinely needs RE (can't build from a spec):**
- ~~**Fan control algorithm**~~ ☑ **docs/42** — sensor→PWM grouping (SINGLE/FASTEST/AVERAGE), per-PWM
  Min/Max/Hyst/Blow/Lock; **fail-safe = force MAX on sensor-fault, OS-shutdown on critical**. (NOT PID;
  PID is power-cap.) Demand curve + SDR thresholds + groupings = board data (RE on HW / SDR dump).
- ~~**Power-capping / regulator**~~ ☑ **docs/43** — Power Regulator (4 P-state modes: OS/Dynamic/
  StaticLow/StaticHigh) + power-cap PID (the BURN PID over ROM-CHIF → CPU F-stop). iLO=controller,
  BIOS=actuator. (Fan/thermal is the BIOS's, not iLO's — docs/42 corrected.)
- **FUM** firmware-update flow (88) — component types, flash staging, the signed-component format.
- ~~**IRC / KVM streaming protocol**~~ ☑ **docs/45** — P84 wire protocol (dvcnet/p84/irccmd/dvcrcv)
  mapped: tasks, muxed channels, transport (raw TCP + AES/RC4, port 17990), sessionKey auth,
  shared-console lock, licensing, clipboard. **Replacement chosen: docs/46** — drop the EOL
  Java/.NET IRC + P84 socket, adopt **RFB (VNC) + noVNC** over WebSocket/TLS (Hextile encode,
  software-ARMv5-sized), reusing capture (14)/HID (29)/clipboard (45). Deferred behind core BMC.
- **VSP** virtual-serial-port passthrough (111) — host-serial-over-CHIF specifics.
- **ERS** Insight Remote Support / phone-home (44) — probably skip for a clean rewrite.
- **Language packs** (122) — replaced by the new UI's i18n; no RE.

### Priority for a *minimal, safe* BMC
1. ~~Fan control algorithm (docs/42)~~ ☑ — model done; demand curve/thresholds = board data (HW/SDR).
2. ~~Power-cap/regulator (docs/43)~~ ☑ — Power Regulator modes + cap PID (ROM-CHIF). Fan=BIOS (docs/42).
3. NTP/syslog/SMTP + LDAP config — standard, straightforward.
4. Everything else (SSO/2FA/WS-Man/ERS/IRC/VSP) — feature-by-feature, as scope grows.

### Verdict — is the whole binary RE'd?
**No, not byte-for-byte — and it doesn't need to be.** Of the 12 MB: web-UI assets + 3rd-party
libs (RTOS/Allegro/SSL/klite/TCP-IP/XML) + language packs are **replaced wholesale**, not RE'd.
The **core is fully mapped** — all hardware (register-exact, docs/02-32), every management service
(contract-level, docs/33-43), and the iLO↔BIOS interface (docs/44). **That is enough to build the
rewrite.** Remaining gaps are small/scoped: **FUM** self-update (26 hits); **remote-console** wire
protocols (IRC/vMedia/VSP — only if KVM is in scope, heavy on ARMv5, likely deferred); **blade/OA**
(309 — out of scope for a DL380p **Gen7 rack** server); **AHS** format (skip); Kerberos impl (standard).
RE these just-in-time per feature.
