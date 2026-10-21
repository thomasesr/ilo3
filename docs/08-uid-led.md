# iLO 3 UID / LED control — what bus drives what

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`ARM:LE:32:v5`, base 0).
Answer to "is the UID LED on I2C?": **the system/chassis UID LED is driven via the CPLD, not
I2C. Only the per-drive-bay UID LEDs are I2C.**

## System / chassis UID LED (the blue locator + button) — **CPLD**

`src\uid.c` manages the UID state and drives the LED through the **CPLD**, not I2C or a direct
GPIO:

- uid.c's hardware register (`@0x2d0144`) = **`0x01F14000`** — the OS-virtual MMIO block that
  interfaces the CPLD (same block as PWRMICRO/power-manager, see `docs/05`). It reads a CPLD
  byte and tests **bit6 (`0x40`)** for the UID state (`and r6,r0,#0x40`).
- `uid: CPLD value is: %02X`, `iod_cpld: WriteIODeviceRegister` — the CPLD is an INTEGRITY
  IODevice; LED/state writes go through it.
- **The UID button is a CPLD interrupt source**: `CPLD BUTTON PRESSED interrupt` /
  `… RELEASED interrupt`, `Notifying system of CPLD interrupt, source mask = %02X`,
  `Get CPLD Inputs`. So pressing the front-panel UID button raises a CPLD IRQ → iLO toggles UID.
- The CPLD itself is **SPI-attached** (`src\cpld_spi.c`, `cpld_spi_dispatch`, `CPLDSPI`,
  `CPLD SPI flash completed`) — that path is for updating the CPLD's own logic; runtime
  LED/button access is the memory-mapped `0x01F14000` register + `iod_cpld`.
- CPLD has multiple PALs: `CPLD - PAL0..PAL5`, `iLO CPLD pal_version`.

## General (health/status) LEDs — ASIC register banks (GPIO-like), not I2C

The `eh_led`/SDR framework (`led: SDR %u`; `Index 1,2,3 -> UID, IH, XH`) drives LEDs by
read-modify-write of **memory-mapped ASIC register banks**, selected per SDR record
(`FUN_0x111f40`: `ldrb [bank,idx]`; set/clear/and/or; write back). Bank pointers:
`0x01F00200` (fan/thermal/control block) and `0x01F14000` (PWRMICRO/CPLD block). No I2C in this
path.

## Per-drive-bay UID LEDs — **I2C** (+ SGPIO)

Disk-drive locator LEDs are the only UID LEDs on I2C:
- `htdrv_set_drive_uid - … i2c_addr: %x`, `drives_set_uids: i2c_addr`,
  `health/drives_get_backplane_i2c_addr`, `UID(bay uid i2c)` — I2C to the drive backplane /
  storage-enclosure processor (SEP).
- `health/drives_get_sgpio_data` — SGPIO also used for drive activity/locate LEDs.

## Summary for the OpenBMC port

| LED | Transport | iLO3 access |
|-----|-----------|-------------|
| System/chassis **UID** (blue locator) + button | **CPLD** | MMIO `0x01F14000` + `iod_cpld`; button = CPLD IRQ |
| Health / status LEDs | ASIC register banks | MMIO `0x01F00200`, `0x01F14000` (SDR RMW) |
| Per-drive **UID** / activity | **I2C** + SGPIO | backplane SEP, `i2c_addr` |

For an OpenBMC port: the chassis UID LED maps to a **CPLD GPIO/register** (model as a
`gpio`/`led-gpio` or a small CPLD driver behind `0x01F14000`), with the UID button as a CPLD
interrupt — *not* an `i2c` LED. Drive-bay LEDs map to I2C/SGPIO backplane devices.
