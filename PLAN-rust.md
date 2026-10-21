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
     ☐ `embassy-time-driver` over the timer (real `Timer::after`; rides on the Wallace tick IRQ) ·
     ☐ MMU bring-up (ARM926 vector base `0x0`/`0xFFFF0000` → unblocks live IRQ delivery) ·
     ☐ SPL (DDR init + SPI-NOR load) · ☐ hardware validation (board / custom QEMU machine).
2. **Platform HAL** — I2C, SPI-NOR, PECI, CPLD-gpio, fan scan-chain, MAC+MDIO (port the other 5
   drivers). Sensor/power/fan/PSU control (`docs/08`,`09`,`20`,`22`-`27`).
3. **Networking** — `smoltcp` on the GXE MAC; DHCP/DNS/SNTP/syslog (RFC crates).
4. **Crypto/TLS** — `rustls` + a **software entropy pool** (timer-jitter CSPRNG, `docs/32`);
   cert/key store (PKCS8/PEM in flash).
5. **Management services** — IPMI (KCS), SSH CLI, SNMP agent, a minimal HTTPS status+control UI.
6. **Host interface** — CHIF server (`docs/28`), SMBIOS inventory, host power/state.
7. **(hard, optional)** vUSB/vMedia (`docs/29`) + KVM video (`docs/14`) — likely too heavy on
   250 MHz ARMv5; defer / may stay unsupported.

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
