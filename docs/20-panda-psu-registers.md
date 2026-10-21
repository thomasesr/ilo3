# iLO 3 "panda" PSU register map (PANDEMONIUM-class supplies)

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`src\ps_panda.c`,
`ARM:LE:32:v5`, base 0). How iLO 3 reads PSU wattage/voltage/current/health over I2C.
Extends `docs/19`. The driver family is **PANDEMONIUM** ("panda"); supplies are smart
I2C devices, one per bay (bus/addr from the bay struct, `docs/19`).

## Transport — `pwrsupply_panda_read(bus, addr, reg, *out_u16)` @ `0x45912c`

SMBus-style 16-bit register read with an HP checksum (NOT standard SMBus PEC):

1. Write **2 bytes**: `[reg, cksum]` where `cksum = (0x100 - reg - addr) & 0xff`.
2. Read **3 bytes**: `[lo, hi, pec]`.
3. Validate `(lo + hi + pec) & 0xff == 0` (simple sum check).
4. Result `u16 = (hi << 8) | lo`.
5. **3 retries** with a ~50 ms delay on checksum/bus error (`Checksum error. Retries %d …`).

`pwrsupply_panda_write` @ `0x45925c` is the mirror (reg + 16-bit data + checksum).
`pwrsupply_panda_register_get(bay, reg, *out)` @ `0x45937c` is the high-level wrapper:
looks up the bay's I2C bus/addr, checks present (`flags & 1`) + capable (`flags & 0x20`),
then calls `panda_read`. Raw values pass through `pwrsupply_panda_value_adjust` @ `0x459d4c`
(per-supply calibration/scale) for the measured quantities.

## Register map (reg = SMBus command byte)

| Reg | Quantity | Confidence | Source |
|-----|----------|------------|--------|
| `0x00` | **firmware revision** (u16 → `maj.min`) | CONFIRMED | `panda_firmware_rev_get`, also `identify` probe |
| `0x02` | **status word** — bits **[10:8]** = supply state/fault code | CONFIRMED | `panda_health_get` (`lsr #8; and #7`) |
| `0x08` | measurement | getter | `register_get(0x08)` @ `0x45a120` |
| `0x0A` | **input current** (raw ×k>>6, +500, /1000 → A) | CONFIRMED | `panda_input_current_get` (`raw = %04X -> %d`) |
| `0x0C` | measurement (via `value_adjust`) — **input voltage / power** | strong-inferred | `register_get(0x0C)` @ `0x45a270` |
| `0x0E` | measurement (high byte taken) | getter | `register_get(0x0E)` @ `0x45a1cc` |
| `0x12` | measurement (via `value_adjust`) — **input power / output** | strong-inferred | `register_get(0x12)` @ `0x45a310` |
| `0x2A` | health/aux | getter | `panda_health_get` 2nd read @ `0x459ba0` |
| `0x2C`,`0x2E`,`0x30` | **instantaneous telemetry triple** (V/I/P); `0x2C` re-read for stability | reader | `0x459e08` (reads 2E,2C,30, re-reads 2C) |
| `0x32` | **peak power** | CONFIRMED | `panda_peak_pwr_get` (`register_get` + `value_adjust`) |
| `0x3A` | **control register** (R/W) — high-efficiency-mode / standby step-down | CONFIRMED | `panda_hem_standby_set` (read-modify-write) |
| `0x56` | aux/extended | caller | `panda_read(0x56)` @ `0x45aaa4` |

Identity (model / part-number / serial) is separate: read from the PSU **FRU EEPROM** at its
own I2C address (`static_status->i2c_fru`, `docs/19`), not from these registers.

## Key conclusion for the OpenBMC port

**This is an HP-proprietary register map, NOT PMBus.** The command bytes don't match PMBus
(PMBus `READ_IIN=0x89`, `READ_VIN=0x88`, `READ_PIN=0x97`, `STATUS_WORD=0x79`,
`MFR_REVISION=0x9B` — none line up with `0x00/0x02/0x0A/0x0C/0x2C…`). Consequences:

- OpenBMC's generic PMBus PSU path (`phosphor-power` PMBus / `dbus-sensors`) **will not bind**
  to these supplies as-is. A port needs either:
  1. a **custom "panda" PSU driver/daemon** reproducing the `[reg,cksum]`→`[lo,hi,pec]`
     transaction and this register map on the `i2c-gxe` adapter (buses 0-5, `docs/16`), or
  2. verification that the Gen7 PSUs *also* expose a standard PMBus page (many HP supplies do)
     — then use the stock PMBus stack and ignore the proprietary map.
- The checksum (`0x100-reg-addr` out, sum-to-zero in) and 3-retry policy must be reproduced.
- Presence/enable still come from the **CPLD-gpio** (`+0x19`/`+0x18`, docs/09); a line read as
  "present + capable" (`flags & 0x21`) gates whether to probe the supply.

## Open

- Exact assignment of `0x0C`/`0x0E`/`0x12` and the `0x2C/0x2E/0x30` triple to
  voltage vs power vs VA — needs the `value_adjust` scale constants and the
  `pwrburn_total_pwrsupply_input_power_get` aggregation (`Watts/mAmps/Volts/VA`, accum/ps divisors).
- Whether a standard PMBus page coexists (would simplify the port a lot).
- The `pwrmicro`/"ELECTRA" path (`0x465858`, regs 2/4 at `0x44edbc`) is a *separate*
  power-micro register interface, distinct from the per-PSU panda bus.
