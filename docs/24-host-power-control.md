# iLO 3 host power control — power button, on/off/reset/NMI

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`src\hostpwr.c`,
`ARM:LE:32:v5`, base 0; code addr = file offset). Register writes/bits CONFIRMED by
disassembly. All addresses physical (identity-mapped, docs/10).

## Register blocks

- **Host-interface `0x01F02000`** (docs/06): `+0x46` = virtual PWRBTN#/WAKEUP# line;
  `+0x70` = 32-bit power-state monitor.
- **Host-power `0x01F00000`**: `+0x5C`/`+0x5D` = warm-reset actuator; `+0x6E` = NMI;
  `+0xE7` = CSM cold-boot command.
- **CPLD/PWRMICRO `0x01F14000`** (docs/09, `iod_cpld`): `+0x04`/`+0x05` = host-power
  request (RMW, 2=request); `+0x08` = button input; CPLD IRQ source-mask bit `0x10`.

## Actuation — the one primitive + the commands

`press_vbutton(ms)` @ **`0x448910`** is the single actuator for ON / graceful / force-off:
`strb 0 → 0x01F02046` (clear), `strb 1` (press), delay `ms×1000 µs`, `strb 0` (release).
`0x01F02046` drives the host **PWRBTN#/WAKEUP#** line. Graceful-vs-force = hold **duration**
(ATX convention: short press = ACPI graceful, long hold = force), not a different register.

| Action | Register | Write | Function | Conf |
|--------|----------|-------|----------|------|
| **Power ON** (momentary) | `0x01F02046` | 1 → hold 100 ms → 0 | `0x448f64` (`press_vbutton`) | CONFIRMED |
| **Graceful shutdown** | `0x01F02046` | 1 → short hold → 0 | `hostpwr_vbtn_press_sync` `0x4489a0` | CONFIRMED (dur=inferred) |
| **Force off** | `0x01F02046` | 1 → hold **≥6 s** → 0 (pre-sets CPLD `+0x05`=2) | `0x4494b0` | CONFIRMED |
| **Warm reset/restart** | `0x01F0005C`←0, `0x01F0005D`←**0x80** | 2-byte seq | `hostpwr_transition_command_reset` `0x4498e0` | CONFIRMED |
| **NMI** | `0x01F0006E` | set **bit2 (0x04)** → hold 1 s → clear | `hostpwr_transition_command_nmi` `0x44a010` | CONFIRMED |
| **Force cold-cycle (CSM)** | `0x01F000E7` | byte **0xB2** | `hostpower_shutdown` `0x4497fc` | CONFIRMED |

High-level entry: `pwrmgr_api` → enqueues `HOSTPWR_TRANSCMD_{BUTTON_PRESS, RESET_THE_BOX,
NMI_THE_BOX}` (APIs `PWRMGR_API_PWR_ON/OFF/FORCE_OFF`, dispatch `0x444e3c`/`0x444e80`).

## Physical power-button READ

- **Interrupt**: CPLD IRQ, source-mask **bit `0x10`** (`tst r1,#0x10` @ `0x2d23ac`) in the
  CPLD dispatcher (`Notifying system of CPLD interrupt, source mask = %02X`, `0x2d1e4c`).
- **State**: CPLD reg **`0x01F14008`** via `iod_cpld` read (`0x1a3ee94` @ `0x2d23c4`), test
  **bit6 (0x40)** → set = PRESSED, clear = RELEASED. Handler `0x2d23ac`.
- **Bridge**: on physical press iLO asserts the virtual button on the host's behalf
  (`eventlistener_task … asserting virtual power button on behalf of physical button`).
  Caveat: the generic "CPLD BUTTON" source is shared with UID-button logic; power = source
  bit `0x10` in this handler (vs UID = `+0x00` bit6, docs/08).

## Power-good / power-state READ

Getter `0x44abc4` reads 32-bit **`0x01F02070`**:
- **bit24 (0x01000000)** = power-on/PGOOD. Clear ⇒ OFF (returns 0).
- **bit25 (0x02000000)** = reset-active. Powered+bit25 ⇒ in-reset (−1), else ON (1).

Level events `EVT_PGOOD`/`EVT_PCIRST` via `hostpwr_monitor`; CPLD mirrors request/state at
`0x01F14004`/`0x01F14005`.

## OpenBMC port mapping (→ `x86-power-control`)

`x86-power-control` drives **libgpiod GPIO lines**; iLO's signals are **MMIO register bits**
on the GXE host-iface/host-power/CPLD blocks, not SoC GPIO pads — so each is a register bit:

| x86-power-control | iLO source | port action |
|-------------------|-----------|-------------|
| `POWER_OUT` (PWRBTN#) | `0x01F02046` =1/0 timed | net-new timed pulse (MMIO) or gpio-mmio output line |
| `POWER_GOOD` | `0x01F02070` bit24 | gpio-mmio input line |
| `RESET_OUT` | `0x01F0005C`=0,`0x01F0005D`=0x80 | net-new 2-byte pulse (not one line) |
| `NMI_OUT` | `0x01F0006E` bit2 (1 s) | gpio-mmio output + timed pulse |
| `POWER_BUTTON` (phys in) | CPLD `0x01F14008` bit6 + IRQ src `0x10` | line off CPLD regmap/irqchip (cpld-gpio, docs/09) |
| force cold-cycle | `0x01F000E7`=0xB2 | no stock analogue — custom hard-cycle action |

**Net-new**: a small `gpio-mmio`/custom gpiochip over `0x01F00000`/`0x01F02000`, plus the
`cpld-gpio` block (docs/09) for the button, feeding `x86-power-control`. The warm-reset
2-byte sequence and the `0xB2` CSM cold-boot have no stock GPIO/sequencer equivalent — model
them explicitly. `0x01F02046` (PWRBTN# out) + `0x01F02070` bit24/25 (PGOOD/in-reset in) are
the clean GPIO lines.
