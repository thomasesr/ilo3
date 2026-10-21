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
- **`embassy`** async executor; HAL modules = Rust ports of the 9 drivers already written.
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
- **ARMv5 Rust**: tier-3-ish; `core::sync::atomic` needs `critical-section`/`portable-atomic`
  (no native LDREX/STREX pre-ARMv6). Expect this friction early in bring-up.
