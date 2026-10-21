# iLO 3 — where PSU wattage / health / identity come from

Date: 2026-10-20. Static RE (strings + call-graph) of `decompiled/os_main_base0.bin`.
Answer to "where does PSU wattage and health status come from?": **from the power
supplies themselves over I2C** (smart PSUs), plus a **FRU EEPROM** per bay — NOT from
the CPLD. The CPLD only supplies the discrete presence/enable/bay-count signals.

## Three distinct sources

| Datum | Source | Evidence |
|-------|--------|----------|
| PSU **presence**, **enable**, **bay count** | **CPLD** `0x01F14000` (`+0x19` present, `+0x18` `EXPBUS_PS_ENABLE`, `+0x1A` bays) — discrete bits | docs/09; `pwrsupply_match_check writing EXPBUS_PS_ENABLE = %02X` |
| PSU **identity** (model, part number, serial, fw rev) | **FRU EEPROM** over I2C | `static_status->i2c_fru`, `FRU EEPROM`, `pwrsupply_faulty_partnumber_get`, `pwrsupply_panda_firmware_rev_get` |
| PSU **telemetry** (input power W, Vin, Iin, VA, capacity) + **health/status** | **PSU registers over I2C** (PMBus-like) | `pwrburn_total_pwrsupply_input_power_get: Watts=%d, mAmps=%d, Volts=%d, VA=%d`; `pwrsupply_panda_read/write`, `pwrsupply_get_register`, `pwrsupply_panda_health_get` |

## The PSU I2C driver — "panda" (PANDEMONIUM family)

iLO probes and talks to each supply as an I2C device:

- `pwrsupply_identify: Identifying Power Supply at i2c bus %d, address %02X` →
  `Identifed PANDEMONIUM-class Power Supply at i2c bus %d, address %02X`
  (`POWER SUPPLY IS UNRESPONSIVE` on failure). PSU family = **PANDEMONIUM** → the
  register driver is `pwrsupply_panda_*` ("panda").
- Register access: `pwrsupply_panda_read` / `pwrsupply_panda_write`,
  `pwrsupply_get_register`, `pwrsupply_panda_hem_standby_set()` ("Failed to read/write
  control register") — read/modify-write of PSU registers over I2C (PMBus-style).
- Per-bay I2C coordinates live in the bay struct:
  `power_supply_bay[N].status.static_status.i2c_bus` / `.i2c_address` / `.i2c_fru`.
  The **power micro ("ELECTRA" / PWRMICRO)** mediates: `pwrmicro_i2c_bus < 6`,
  `pwrmicro_electra_max_power_get: reading max power` — so PSU I2C is on buses **0-5**
  of the 8 GXE I2C engines (docs/16), reached through the power-micro.

## Derived values

- **Input power / current / voltage / VA** per supply from `panda_read`, summed in
  `pwrburn_total_pwrsupply_input_power_get` (`accum divisor`, `ps divisor`). This feeds
  the web UI "Average Power" / "Present Power Reading" (`last_avg_pwr_accum`).
- **Capacity**: `pwrsupply_capacity_recalc ... total capacity = %d Watts`
  (`waiting on power supply capacity and health for bay %d`).
- **Health**: `pwrsupply_panda_health_get` (`supply in bay %d is not supplying power`),
  faults decoded from PSU status bytes into text: `VREF Failure`, `DAC Failure`,
  `Voltage Channel Failure`, `Calibration Table Invalid`,
  `Input Power Loss or Unplugged Power Cord`, `Over-current fault`, `Micro-controller
  fault`, `Fan N failed or removed`, `Temperature at caution/critical level`.
- Faulty/unsupported supplies fall back to `pwrsupply_faulty_*` getters (no telemetry).

## For the OpenBMC port

- PSU telemetry + identity = a **PMBus / I2C** problem, not a CPLD one:
  - Each PSU bay = an I2C device on one of the GXE I2C engines (buses 0-5), driven by the
    `i2c-gxe` adapter (docs/16). Model in OpenBMC with **`phosphor-power` / `psu-manager`**
    + **`dbus-sensors`** PMBus, and the per-bay **FRU EEPROM** via `entity-manager`.
  - Presence / enable / bay-count stay on the **CPLD-gpio** driver (`+0x19`/`+0x18`/`+0x1A`,
    docs/09) — presence gates whether to probe the PMBus device; `EXPBUS_PS_ENABLE` is the
    per-bay enable line.
- **Register map recovered → `docs/20`**: the "panda" protocol is an HP-proprietary SMBus
  register read (`[reg,cksum]`→`[lo,hi,pec]`), **NOT PMBus**. Regs: 0x00 fwrev, 0x02 status,
  0x0A input current, 0x32 peak power, 0x3A control, + measurement regs 0x0C/0x0E/0x12/0x2C-0x30.
