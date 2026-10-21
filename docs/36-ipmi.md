# iLO 3 IPMI — command surface for the clean-room rewrite

Date: 2026-10-21. Static RE of `os_main_base0.bin`. Maps the IPMI interface so the Rust rewrite
can reimplement it. IPMI 2.0 + DCMI are **public specs** (build to spec); only the **HP OEM
commands** need RE from the binary. Transport is already mapped: KCS (`docs/05`), host PCI/CHIF
(`docs/06`,`28`).

## Transports

- **KCS** — host-side system interface at `0x01F0D400` (`docs/05`). Standard KCS state machine:
  `KCS_S_IDLE / WRITE / READ_CONTINUE / ERROR`, `KCS_GET_STATUS_ABORT`. → `phosphor-ipmi-kcs`
  equivalent / a Rust KCS driver.
- **IPMI-over-LAN** — RMCP/RMCP+ on the network (`IPMI_DCMI_OVER_LAN`). Build to spec.
- **Host PCI messaging** — `IPMIPCISR` (the `.2` CHIF function, `docs/06`); host OS ↔ BMC IPMI
  over the shared-mem transport.
- Service task: `IpmiService` / `IPMI_SVC` / `IPMIMsgQ`; privilege model `IpmiPrivillege`.

## Standard IPMI 2.0 command set (build to spec)

Confirmed present (string evidence): **App** — `Get Device ID`, `Get Channel`, `Get/Set User`,
`cold reset`, `Send Message`, `Master Write-Read`, `Set Watchdog`. **Chassis** — `Chassis
Control`, chassis status (`CHASSIS_OPEN`). **Sensor/Event** — Get Sensor Reading, SDR repository
(`SDR_ARRIVED/DISC/RDY`, Reserve/Get SDR), **SEL** (`get_sel`/`add_sel`). **FRU** — `get_fru`,
`FRU_ASSET_TAG_CHANGE`, `FRU_MAX`. Sensor types incl. `IPMI_SENSOR_DRIVE_BAY`,
`IPMI_POWER_SUPPLY`. Completion codes e.g. `IPMI_CC_OUT_OF_RANGE`.

These map onto data already RE'd: sensors → `docs/25`, PSU/power → `docs/20`/`24`, drives →
`docs/23`, inventory → `docs/22`. SDR/SEL = a record store the rewrite owns.

## DCMI 1.5 (Data Center Manageability Interface) — full support

`dcmi_*` handlers (build to the DCMI spec, NetFn 0x2C group `0xDC`):
`dcmi_get_caps`, `dcmi_get_power_reading`, `dcmi_get_power_limit`, `dcmi_set_power_limit`,
`dcmi_toggle_power_limit`, `dcmi_get_asset_tag`, `dcmi_set_asset_tag`, `dcmi_get_ctlr_id_str`,
`dcmi_set_ctlr_id_str`, `dcmi_get_dcmi_sensor_info`, `dcmi_summary`. Power modes:
`DCMI_PWR_READING_MODE_SYSTEM` / `_ENHANCED_SYSTEM`. Power data comes from the PSU/power path
(`docs/20`,`24`).

## HP OEM commands (proprietary — RE just-in-time)

OEM groups (strings): **`OEMHP_POWER`** (power management OEM), **`OEMHP_RC`** (remote console),
**`OEMHP_VM`** (virtual media). HP/Compaq enterprise IANA (HP=11, Compaq=232/`0xE8`). These carry
iLO-specific control (power capping extras, RC/vMedia session control) beyond standard IPMI. The
exact **NetFn + command bytes** need disassembly of the OEM dispatch (`cmd_handler`/`ipmi_cmd`)
when implementing — not required for host-OS interop (which uses standard IPMI/DCMI).

## Rust rewrite mapping

- An **IPMI message core** (session/auth/privilege) + command handlers, fed by the KCS driver,
  a LAN (RMCP+) listener on `smoltcp`, and the CHIF host path (`docs/28`).
- **Priority**: standard IPMI 2.0 (chassis power, sensors, SEL, FRU, user) + DCMI — these are what
  host OSes and `ipmitool`/`freeipmi` actually use. Build to spec; reuse the mapped data paths.
- **HP OEM** (`OEMHP_*`): optional; implement only if HP-tool compatibility is wanted — RE the
  command codes then.
- No ready IPMI-responder crate for `no_std`; implement the command handlers directly (the
  command set above is the contract). Keep SDR/SEL as a compact Rust record store in NVRAM/flash.

## Open

- HP OEM NetFn/command byte codes for `OEMHP_POWER/RC/VM` (disasm `cmd_handler` dispatch).
- The exact SDR/SEL record formats iLO exposes (standard IPMI SDR types; RE field specifics JIT).
- IPMI-over-LAN auth (RMCP+ cipher suites) — standard, but confirm which suites are enabled.
