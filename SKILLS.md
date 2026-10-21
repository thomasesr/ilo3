# SKILLS.md — clean-room iLO 3 → Rust firmware: build reference

This file holds (1) the **hardware constraints** every design decision must respect, (2) the
**full implementation checklist** (everything to build), and (3) the **optimal Claude Code skills**
to add under `.claude/skills/` to accelerate the work. Companion to `PLAN-rust.md` (roadmap +
RE coverage) and `docs/02`-`41` (the RE spec).

---

## 1. Hardware constraints (the budget — never violate)

| Resource | Value | Consequence |
|----------|-------|-------------|
| **CPU** | **250 MHz ARM926EJ-S** (ARMv5TEJ, 1 core) | no VFP/NEON, no HW div, slow; ARMv5 = Rust tier-3-ish |
| **Atomics** | **no LDREX/STREX** (pre-ARMv6) | `core::sync::atomic` needs `critical-section`/`portable-atomic` |
| **RAM** | **128 MB DDR3, ~60 MB usable** (ECC + 64 MB KVM video reserved) | tiny heap budget; `no_std`, no big allocations |
| **Flash** | **8 MB SPI NOR** (multi-sourced: Micron/Spansion/Atmel/Eon/GigaDevice/ISSI; 3.3 V — VERIFY) | tiny image; consider a 16/32 MB chip swap (`docs/15`) |
| **Crypto** | **software only — NO HW engine, NO HW TRNG** (`docs/32`) | TLS is CPU-bound + slow; must build a software entropy pool |
| **RTC** | **none** — software clock seeded by SNTP (`docs/18`) | time depends on SNTP (`docs/41`); persist last-known |
| **Vectors** | ARM926 **high vectors `0xFFFF0000`** (boot ROM = reset target, `docs/31`) | SPL/boot layout |
| **MMIO base** | `0xC0000000` core block; `0x01F0xxxx` peripheral window | identity-mapped (VA=PA, `docs/10`) |
| **IRQs** | 2-bank, 64 lines (`0xCEFF0000`+`0x80F00000`, `docs/12`) | per-device IRQs recovered |
| **Boot** | reset→flash ROM (no silicon RoT); DDR table + clock strap (`docs/30`) | reflash via SPI; sig-patch `0x7f38bc` (`docs/31`) |

Design rules: `no_std`, `panic=abort`, LTO, size-optimized; cooperative async (`embassy`), no
threads; keep static RAM small; prefer ChaCha20-Poly1305 (no AES HW); SNTP-seed the clock at boot.

---

## 2. Implementation checklist (everything to build)

### Boot / first-stage (SPL)
- [ ] Reset vector + CP15 setup (high vectors `0xFFFF0000`) — `docs/31`
- [ ] Clock/PLL from strap `0x801F00A4` → `0x80200088` — `docs/30`
- [ ] DDR3 init (reg table, warm-check, cal-wait) — `docs/30`
- [ ] SPI-NOR read (SMIF `0xC0000200`) to load the main image — `docs/15`
- [ ] Decompress + jump (or run XIP) — `docs/03`

### HAL (port the 9 RE'd drivers → Rust, `docs/02`-`27`)
- [ ] UART (`0xC00000F0`, polled) · [ ] Timer (`0xC0000098` + Wallace `0xD1000000`)
- [ ] IRQ controller (2 banks, 64 lines) · [ ] I2C (8 engines `0x01F10100`)
- [ ] SPI-NOR (SMIF) · [ ] GPIO/CPLD (`0x01F14000`, UID/power/PSU)
- [ ] PECI (`0x01F00400`, CPU temp) · [ ] Fan scan-chain (PWM/tach)
- [ ] GXELAN MAC + MDIO (`0xC0004000`/`0xC0005000`, NC-SI) — **TX/RX ring format HW-confirm**
- [ ] Crypto = software (no driver) — `docs/32`

### Core services / sensors (data paths in `docs/08`,`09`,`20`,`22`-`27`)
- [ ] Sensor read (temps: PECI + I2C LM75; fans: tach; voltages/VRM)
- [ ] **Fan control algorithm** — ⚠️ SAFETY-CRITICAL, **RE still needed** (`docs/42` todo)
- [ ] Power control (on/off/reset/NMI/CSM) + state machine — `docs/24`
- [ ] PSU telemetry (panda SMBus) — `docs/20` · [ ] UID/health LEDs — `docs/08`
- [ ] Power capping/regulator algorithm — **RE still needed** (`docs/43` todo)
- [ ] Inventory (CPU/mem via host SMBIOS over ROM-CHIF) — `docs/22`
- [ ] Drive backplane (SEP I2C frames) — `docs/23`

### Networking (`smoltcp`) + standard services (`docs/41`)
- [ ] TCP/IP + DHCP/DNS · [ ] **SNTP** (seeds the clock — high) · [ ] syslog · [ ] SMTP alerts
- [ ] **LDAP + Kerberos** directory auth (schema-free group→role) · [ ] mDNS/SLP discovery

### Security / crypto (`docs/32`)
- [ ] **rustls** TLS (ChaCha20 preferred) · [ ] **software entropy pool** (timer-jitter CSPRNG)
- [ ] Cert/key store (PEM/PKCS8) + CSR gen (`rcgen`) · [ ] 2FA / smartcard mTLS

### Management surfaces (contracts in `docs/33`-`39`)
- [ ] Web server (HTTP + rustls) + **58 `json/*` endpoints** + a minimal SPA — `docs/33`
- [ ] **Redfish** subset (modern primary API)
- [ ] SSH (`russh`, modern ciphers) + **SMASH CLP** engine (`/map1`, `/system1`) — `docs/34`
- [ ] **SNMP** v1/v2c (+add v3) agent, CPQ MIBs + traps — `docs/35`
- [ ] **IPMI** 2.0 + DCMI responder (KCS/LAN/CHIF) — `docs/36`
- [ ] **RIBCL** XML handler (legacy HPONCFG compat) — `docs/37`
- [ ] NVRAM config store (K-V, wear-leveled) + user DB (≤12, argon2, privilege bitmap) — `docs/38`
- [ ] Event framework (EventBus) + IML/iLO/security logs (coalescing, circular) — `docs/39`

### Host interface (hardest; `docs/06`,`28`,`29`,`14`)
- [ ] CHIF shared-mem server (doorbells, CCB ownership, magic/checksum) — `docs/28`
- [ ] SMBIOS-over-ROM-CHIF intake — `docs/22`
- [ ] (optional, heavy) vUSB/UHCI emulation + vMedia — `docs/29`
- [ ] (optional, heavy) KVM video capture + IRC stream — `docs/14`

### Firmware update (FUM)
- [ ] Self-update flow + component inventory + signed-component format — **RE still needed**

---

## 3. Optimal Claude Code skills to add (`.claude/skills/<name>/SKILL.md`)

Each skill packages a recurring workflow so it's applied consistently. Listed by payoff.

### `arm-re` — ARMv5 reverse-engineering workflow
**Use when**: extracting a register map, function, or wire-format from the iLO binaries.
**Encodes**: capstone `ARM:LE:32:v5` setup; the **base-0 pointer trick** (a 32-bit word == a
string/table file-offset is a pointer); **ADR resolution** (`add/sub rX,pc,#imm`); literal-pool
scanning; **phase-1 alignment** (drop-1-byte for the main OS); finding MMIO by base+offset vs
literal; the "disasm derails on pools" gotcha. Inputs: `decompiled/*.bin` + base addresses.
Output: a `docs/NN` with evidence (string offset + code addr) + confidence tags.

### `no-std-armv5` — bare-metal Rust for ARM926EJ-S
**Use when**: setting up or building any crate in `rebuild/ilo3-rs/`.
**Encodes**: the custom `armv5te` target JSON; `#![no_std]` + `panic=abort` + LTO + size opts;
**`critical-section`/`portable-atomic`** for the no-LDREX atomics; linker script for high-vectors
`0xFFFF0000` + the memory map; `embassy` executor init; early UART console. Flags the known
ARMv5 Rust tier-3 friction up front.

### `reg-map-to-hal` — register map → Rust HAL module
**Use when**: turning a `docs/NN` peripheral map into a driver.
**Encodes**: volatile MMIO read/write wrappers, `bitflags` for register fields, the probe/init +
op pattern (mirroring the 9 Linux drivers), MMIO-safety invariants, and the CONFIRM-ON-HARDWARE
comment convention for RE-uncertain fields.

### `clean-room-port` — clean-room discipline gate
**Use when**: implementing any subsystem that has an HP original.
**Encodes**: the facts-vs-expression split (register maps/wire formats = facts, usable; HP code
= never copied); for a subsystem, work from the `docs/NN` contract + the RFC, cite the spec, and
record provenance. A checklist that keeps the rewrite legally clean (interop/repair, owned HW).

### `protocol-to-spec` — implement a standard protocol in Rust
**Use when**: building IPMI/SNMP/SSH/NTP/SMTP/syslog/LDAP (`docs/34`-`37`,`41`).
**Encodes**: pick the crate (russh/smoltcp/rustls/ldap3/lettre/sntpc), build only the enabled
feature subset (from the RE notes), wire it to the HAL/service data + EventBus, enforce the
privilege bitmap. Modern-only (drop SSH-1/3DES, add v3/mTLS).

### `bmc-service-wiring` — one datum, all surfaces
**Use when**: exposing a sensor/setting/action.
**Encodes**: wire one data source through web-JSON + Redfish + SMASH-CLP + IPMI + SNMP + RIBCL
consistently, from a single typed model + the NVRAM store + EventBus — avoids per-interface drift.

### `spi-reflash` — hardware flash + recovery
**Use when**: reading/writing the boot flash on real hardware.
**Encodes**: identify the chip (flashrom JEDEC, `docs/15`); **verify 3.3 V vs 1.8 V** before
clipping; CH341A/Dediprog + SOIC-8; **dump + back up first**; apply the sig-patch `0x7f38bc`
(`docs/31`); the recovery path. Safety-first (wrong voltage = dead chip).

### `fw-size-budget` — fit the 8 MB flash / 60 MB RAM
**Use when**: adding features or before a release build.
**Encodes**: measure image + static RAM; LTO/strip/`opt-level="z"`; avoid heap growth; `cargo
bloat`/`twiggy`; flag when nearing the flash/RAM ceiling; suggest the flash-chip upgrade if needed.

### (optional) `ddr-bringup` — SPL DDR/clock bring-up
Fold into `no-std-armv5` or keep separate: reproduce the `docs/30` DDR table + clock-strap
sequence as Rust SPL, with the warm-check / cal-wait / MEMCFG steps and HW-confirm notes.

### Priority to create first
1. `arm-re` (still RE'ing — fan algo etc.) · 2. `no-std-armv5` (unblocks all Rust) ·
3. `reg-map-to-hal` (the HAL port) · 4. `clean-room-port` (legal hygiene, every subsystem) ·
5. `spi-reflash` (the one hardware-safety gate).
