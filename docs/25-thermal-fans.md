# iLO 3 thermal — temperatures & fans

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`ARM:LE:32:v5`, base 0;
code addr = file offset). Key register/string claims verified by disassembly.

## Framing: SDR / access-descriptor cache

Sensors are not read from fixed registers by the consumers. A **health/SDR server** polls
hardware into **RAM sensor caches**; CLI/web/IPMI read cached values through a unified
**access-descriptor** framework. Descriptor transports (enum pretty-printer `0xe285c`,
strings `0xe2aec`): **`CPLD, GPO, SChain, I2C, PECI2C, PECI, I2CEX, PECI2, CARB`**.
Per-transport field layout: I2C=`[addr offs mask set dev]`, PECI=`[seg addr offs mask set]`,
CPLD=`[offs mask set]`. Generic read dispatcher `0x12079c`; handlers `0x111298` (8-bit
sign-extended = °C), `0x10f8c4` (loads CPLD `0x01F14000`). Caches: I2C sensors ×16 `0x8b798`,
**fans ×128 `0x86da0` (stride 0x48)**, SDR/scan-chain `0xb6345`. SDR handlers `eh_temp`,
`eh_fan`, `eh_rfan`.

## Temperatures

| Class | Transport | Register / command | Conf |
|-------|-----------|--------------------|------|
| **CPU** | **PECI** engine | native PECI engine **`0x01F00400`** (docs/26); `0x01F14068` is only the XNC target MUX; IRQ 60; health req opcode `0x0B` carries `peci.buf` (seg/addr/cmd) | HIGH transport; MED exact PECI cmd byte (GetTemp vs RdPkgConfig) not extracted. `PECI2C` = PECI-over-I2C variant |
| **Board / inlet / ambient** | **I2C → LM75** | LM75 temp reg `0x00` (2-byte); device on I2C bus 0/1/2 of the 8 engines `0x01F10100` | HIGH transport; per-sensor bus+addr are **SDR/runtime**, not code literals (LM75 std 0x48-0x4F) |
| **ASIC / CPLD** | **CPLD** reg | `0x01F14000` (descriptor CPLD) | MED (handler `0x10f8c4`) |

Evidence: `LM75` @`0x91230`; I2C device-type enum `FRU EEPROM / LM75 / 9544 mux (PCA9544) /
9555 IO expander (PCA9555)`; `ASIC supports 8 I2C engines` @`0x27558`.
**Corrects docs/05**: the "temp block ~`0x01F07000`" appears only in the MMU page-map, never
loaded by code — a mapped aperture, not a read temperature register.

## Fans — **CPLD serial PWM scan-chain** (NOT flat PWM registers)

The surprising, confirmed finding: fan PWM + tach ride a **CPLD-driven serial scan chain** (a
GPIO shift register), not a per-fan PWM-duty register array.

- **PWM** = open-drain **FET** outputs; duty = FET-enable bits clocked through the chain; needs
  periodic **RESYNC**. Strings: `CPLD needs RESYNC for PWM scan chain` @`0x276c4`,
  `This platform needs the FAN RESYNC` @`0x123948`, `PWM: hitting FET for off` @`0x121924`,
  `GpioService : slow scan chain clock divisor set` (`src\GpioService.c`), CLI `feton`/`fet`.
- **TACH** = scan-chain input signals `FAN_1_TACH…FAN_6_TACH` (signal table `0xf3d74`): **6
  tach-monitored fans** (cache sized ≤128; actual count SDR-defined). 16-bit tach → fan-record
  `+0x10/+0x11`, fault flag `+0x18` bit5. `tach: %5u - fault detected`, `Enabling TACH`.
- **Register blocks**: CPLD `0x01F14000` (fan ctrl `+0x0B`, docs/09) + fan/GPIO/scan-chain block
  **`0x01F00000`** (fan status/ctrl ~`+0x200`). The addressing unit is a **bit position within
  the scan chain**, not a byte stride (exact bitmap not fully extracted).
- **Control loop** ~`0x118000-0x128000`: temp→PWM map, min/max/hyst, blowout, ramp, per-PWM lock,
  `eh_rfan` redundancy. CLI `t`/`h`/`p N …`/`g`.

## OpenBMC port mapping

- **CPU temp** → `dbus-sensors`/`peci-cputemp` only if the GXE PECI engine is wire-compatible;
  the engine is at CPLD `0x01F14068` (not Intel/ASPEED) → **net-new GXE PECI driver**, then feed
  dbus-sensors.
- **Board temps** → upstream `lm75` + `pca954x` (9544 mux) + `pca953x` (9555) hwmon on the drafted
  **`i2c-gxe`** adapter; the SDR topology (bus+addr) → DT/entity-manager entries is manual (the
  addresses are runtime SDR data, not in the binary).
- **Fans** → `phosphor-pid-control` for the loop, but the PWM/tach source is the **biggest
  net-new driver**: a GXE **"CPLD fan scan-chain" hwmon** (serial FET-duty shift-out + tach
  decode + RESYNC + GpioService clock divisor) exposing standard `pwmN`/`fanN_input`. No
  `aspeed-pwm-tacho`/`pwm-fan` equivalent — a serial scan-chain fan controller is from scratch (DONE → `fan-gxe.c`, docs/27),
  against CPLD `0x01F14000` (+0x0B) and fan block `0x01F00000`.
- **SDR framework** (`eh_*`) → `phosphor-sensor-monitor` + thresholds over the per-bus drivers.

Caveats: exact PECI command opcode + the scan-chain per-fan bitmap are inferred, not
byte-confirmed; per-sensor I2C addresses are SDR data (recover from a live iLO SDR dump).
