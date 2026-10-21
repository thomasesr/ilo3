# iLO 3 system-management register block `0x01F14000` (CPLD + PWRMICRO)

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`ARM:LE:32:v5`, base 0).
`0x01F14000` is the OS-virtual MMIO block that fronts the **system CPLD** and the **power
microcontroller (PWRMICRO)**. Mapped by harvesting every `base(0x01F14000)+offset` access and
labelling each by the touching function's strings. Register *offsets* and R/W/byte width are
from disassembly; *meanings* are inferred from the surrounding strings (confidence: medium-high
for labelled rows).

Block size ≈ `0x200` bytes (offsets seen `+0x00`..`+0x1F6`). Access is 8-bit (`ldrb/strb`)
unless noted. The same registers are also reachable via the `iod_cpld` INTEGRITY IODevice
(`WriteIODeviceRegister`), which the CPLD-interrupt path uses.

## Register map

| Offset | W | Block | Function (inferred) |
|--------|---|-------|---------------------|
| `+0x00` | r/w b | CPLD | **status / UID** — **bit6 (`0x40`) = UID LED/button**, **bit7 (`0x80`) = status** (direct-MMIO bit tests); `uid: CPLD value is: %02X` |
| `+0x01` | r/w b | CPLD | CPLD/FPGA version & SPI status (`src\cpld_spi.c`, `Failed to get FMP/FPGA version`) |
| `+0x02` | r   b | CPLD | CPLD/FPGA version (cont.) |
| `+0x04` | w   b | PWR  | host-power transition / request |
| `+0x05` | r/w b | PWR  | host-power transition (`hostpwr_detected_transition`, `Clearing power request`) |
| `+0x06` | r   w | PWR  | host-power (`hostpwr: RequestResource IOD`, semaphore) |
| `+0x07` | r   b | PWR  | blade power / `memid scan chain` |
| `+0x08` | r   w | CPLD | **interrupt / button** (`iod_cpld`, `CPLD BUTTON PRESSED/RELEASED`) |
| `+0x09` | r   w | CPLD | interrupt / button (cont.) |
| `+0x0A` | r   w | PWR  | host-power **brownout** / power-on delay (`hostpwr_brownout_thr`) |
| `+0x0B` | r/w b | FAN  | fan / thermal control (`fan ?`, `temperatures`, `algorithms`) |
| `+0x0C` | r   w | PWR  | blade config power / host-power transition |
| `+0x0D` | r   w | CPLD | interrupt / button (`iod_cpld`) |
| `+0x18` | r   w | PWR  | **power supply ENABLE** bitmask (`EXPBUS_PS_ENABLE`); presence/enable only — **wattage/health come over I2C, see `docs/19`** |
| `+0x19` | r   b | PWR  | power-supply presence (`pwrsupply_periodic saw present`) |
| `+0x1A` | r   w | PWR  | power-supply bays (`pwrsupply_num_ps_bays <= 4`) |
| `+0x2C` | r   w | CPLD | interrupt **source / mask** (`iod_cpld`, button) |
| `+0x2D` | r   w | CPLD | interrupt source / mask (cont.) |
| `+0x64`–`+0x65` | r b | CPU | (processor / PECI region) |
| `+0x68`–`+0x6B` | r/w b | CPU | **PECI multi-node target MUX** (XNC), bits[5:0] target — NOT the PECI engine; engine = `0x01F00400` (docs/26) |
| `+0x72`,`+0x74` | r/w b | — | unlabeled |
| `+0x1F5`,`+0x1F6` | r/w b | PWR | power manager (`pwrmgr`) |

## Notes

- **UID LED / button** live at `+0x00` (bit6). The front-panel UID button raises a **CPLD
  interrupt** (sources read/cleared at `+0x08`/`+0x2C` via `iod_cpld`); the handler
  (`FUN_0x2d1b80`) reads a source bitmask and dispatches per-bit (`CPLD BUTTON PRESSED` →
  toggle UID). See `docs/08-uid-led.md`.
- The block is a **mux of CPLD + power-micro functions**: CPLD status/version/interrupt
  (`+0x00`–`+0x02`, `+0x08`–`+0x0D`, `+0x2C`), host-power sequencing (`+0x04`–`+0x0C`,
  brownout), power-supply management (`+0x18`–`+0x1A`), fan (`+0x0B`), PECI (`+0x68`), and
  `pwrmgr` (`+0x1F5`).
- The CPLD's own logic is field-updatable over **SPI** (`src\cpld_spi.c`, `CPLDSPI`) — separate
  from this runtime register interface.

## For the OpenBMC port

Model `0x01F14000` as a **syscon / CPLD-interface** node (physical base pending VA→PA, see
`docs/05`). Chassis UID LED = `+0x00` bit6; UID button = CPLD interrupt at `+0x08`/`+0x2C`.
Host-power control, PSU state, and PECI all route through this one block → several OpenBMC
providers (`x86-power-control`, PSU/sensor, `peci`) would sit on top of a Gromit-XE CPLD driver.

## Open

- `+0x00` bits other than 6 (UID) / 7 (status), and the `+0x08`/`+0x2C` interrupt-source
  bits: these are reached via the `iod_cpld` IODevice (register *index*, not a `0x01F14000`+off
  literal), so a bit-mask scan can't recover them — needs tracing `ReadIODeviceRegister(iod_cpld,…)`.
- `+0x64`–`+0x74` precise roles; `+0x1F5` pwrmgr semantics.
- Physical base (OS-virtual here).

## GPIO driver (OpenBMC port)

`rebuild/meta-ilo3/recipes-kernel/linux/files/gpio-gxe-cpld.c` exposes this block as a
bit-addressable `gpio_chip` (`hpe,gxe-cpld-gpio`): **gpio N = byte (N>>3), bit (N&7)** over
the 0x200-byte window → 4096 lines, RMW get/set, software-tracked direction (the CPLD has no
DDR register). Known lines:

| gpio | reg.bit | signal | confidence |
|------|---------|--------|------------|
| 6 | `+0x00`.6 | chassis **UID LED** | CONFIRMED (uid.c read) |
| 7 | `+0x00`.7 | CPLD status | docs/09 |
| 192-195 | `+0x18`.0-3 | PSU enable | inferred |
| 200-203 | `+0x19`.0-3 | PSU presence | inferred |

DT: the `syscon@1f14000` node gains `gpio-controller`/`#gpio-cells=<2>`; the UID LED is wired
`gpios = <&cpld 6 0>` (active-high) under the `gpio-leds` node. **CONFIRM-ON-HARDWARE**: the fw
writes CPLD bits via the `iod_cpld` IODevice (a locking wrapper over this MMIO) — a plain RMW
should drive the pins but may need a hardware mutex on shared bytes. UID **button** = CPLD
interrupt (source `+0x2C`, parent IRQ 31) — not yet wired as a gpio-irqchip; future work.
