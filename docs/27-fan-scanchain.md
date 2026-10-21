# iLO 3 fan controller — CPLD PWM/tach scan-chain

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`ARM:LE:32:v5`, base 0;
code addr = file offset). Register map CONFIRMED by disassembly. This is the hardware
behind the "net-new fan hwmon" (docs/25): a **CPLD-latched serial scan chain**, not a
flat per-fan PWM/tach register array.

## Register map — fan block base `0x01F00200` (CONFIRMED)

| Off | Reg | Role | Evidence |
|-----|-----|------|----------|
| `+0x0C` | **CTRL** | scan-chain control; low 3 bits = mode (`and #0xf8; add #2` → mode 2 = update) | `0x123bc8` |
| `+0x10`..`+0x17` | **DATA** | 8-byte PWM/FET scan-chain shift data (per-fan FET bits, RMW) | `0x123af0` (clear loop ×8), `0x123b20` (`0xFF`→`+0x17`) |
| `+0x18` | **TACH_CTRL** | write `0xFA` = trigger/arm tach sample | `0x122bb8` (`mov r0,#0xfa; str [=0x1f00218]`) |
| `+0x1C` | **TACH_CFG** | second tach config word | `0x122bc8` (`str r0,[r1,#4]!`) |

Plus the **sync strobe in the CPLD** (`0x01F14000` block, docs/09):

| Reg | Role | Evidence |
|-----|------|----------|
| `0x01F1400B` **bit1** | scan-chain **sync/latch strobe** — set, delay, clear = latch the shifted bits | RESYNC fn `0x123b10` (`orr #2`) / `0x123b34` (`bic #2`) |

## RESYNC sequence (`0x123ae4`)

1. Clear the 8 DATA bytes `0x01F00210..0x17` (strb 0, loop ×8).
2. Delay ~100 µs.
3. Set CPLD `0x01F1400B` bit1 (assert latch).
4. Write `0xFF` to DATA `+0x17`.
5. Delay ~100 µs.
6. Clear CPLD `0x01F1400B` bit1 (deassert).

Strings: `CPLD needs RESYNC for PWM scan chain`, `This platform needs the FAN RESYNC`,
`GpioService : slow scan chain clock divisor set`.

## PWM write + TACH read

- **PWM** (fan dispatcher `0x123b50`): per-fan records (16 max, **stride 0x20**; FET bit at
  `+0x15`) are walked; each FET state is pushed into the scan-chain DATA bytes via the SDR
  access-descriptor framework (`0x1208e8` → `0x111f40`/`0x112050`, RMW set/clear of a specific
  **bit position**), then CTRL mode is set and the chain latched (RESYNC). PWM is a **FET on/off
  bit per fan clocked through the chain** — `PWM: hitting FET for off`, CLI `feton`/`fet`.
- **TACH** (`0x122bac`): write `0xFA`→TACH_CTRL, cfg→TACH_CFG, then per-fan (stride 0x20, enable
  `+0x18` bit0) read the 16-bit raw tach from record `+0x10/+0x11` via the descriptor read
  (`0x12079c`); fault flag `+0x18` bit5. `Enabling TACH`, `tach: %08x`, `tach: %5u - fault`.
- **6 tach fans** (`FAN_1_TACH..FAN_6_TACH`), 8 DATA bytes (up to 8 PWM positions); actual count
  is SDR-defined.

## CONFIRM-ON-HARDWARE (flagged in the driver)

1. **PWM semantics**: whether a fan's "duty" is a hardware value latched in a DATA byte, or a
   software-timed FET on/off toggle run by the fan task (the `feton`/"for off" language + the
   per-bit RMW suggest bit-level FET state, with duty modulated in software). The driver models
   per-channel DATA bytes as duty; revisit if the FET is purely on/off.
2. **Per-fan bit/byte position** in the chain = **SDR/board-defined** (the descriptor framework
   carries it), not a fixed formula — describe it per fan in the devicetree.
3. **TACH raw→RPM scale** and the exact result-read register (the value arrives through the SDR
   descriptor `0x12079c`, not a single pinned MMIO) — confirm on hardware.

## OpenBMC port

`rebuild/meta-ilo3/recipes-kernel/linux/files/fan-gxe.c` = a net-new **hwmon** driver
(`hpe,gxe-fan-scanchain`) exposing `pwmN` / `fanN_input` to **phosphor-pid-control**. No upstream
analogue (`aspeed-pwm-tacho`/`pwm-fan` assume flat registers). Needs the CPLD sync bit (via an
`hpe,cpld` syscon phandle) + per-fan chain positions from DT.
