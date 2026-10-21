# iLO 3 interrupt controller (Gromit-XE)

Date: 2026-10-20. Static RE of the type-2 **kernel** (`decompiled/kernel_type2.bin`,
`interruptcontroller.c`/`interruptmapping.c`, `ARM:LE:32:v5`). Addresses are **physical**
(identity-mapped; the IRQ handler runs with FCSE PID=0 — see `docs/10`).

## Architecture

ARM926 IRQ → kernel exception handler **`0xaf73c`** (`sub lr,lr,#4`; saves context; **sets FCSE
PID = 0** → identity/physical context) → branch to the dispatcher **`0x3018`**, which walks the
INTEGRITY interrupt-controller **object tree** (nested/"Bridge IRQ Mapping"), calling each
controller's poll function and indexing a handler table. FIQ/abort handlers sit alongside in the
kernel core region `0xaf6xx–0xaffxx`.

## Two hardware IC blocks — 64 sources (2 × 32)

| Block | Base | Sources |
|-------|------|---------|
| **IC0** | **`0xCEFF0000`** | IRQ 0–31 |
| **IC1** | **`0x80F00000`** | IRQ 32–63 |

Enable is `1 << (irq & 0x1F)`, selecting IC1 when `irq ≥ 32` (`cmp #0x1f; subhi #0x20`). Both
blocks are initialised and masked together at boot.

## Register map (per IC block, offset from base)

| Offset | Access | Register (inferred) |
|--------|--------|---------------------|
| `+0x0C` | r/w | **status / pending** (object base; read in poll, cleared at init) |
| `+0x10` | r/w | secondary status / ack (IC1 path writes here) |
| **`+0x14`** | w | **interrupt enable** — the mask-update routine (`0x3404`) writes `~mask` here (active-high enable); init writes `0xFFFFFFFF` = all masked |
| `+0x30` | r/w | status clear / EOI (read-modify-write at init) |
| `+0x100`–`+0x17C` | w | **per-source config array** — 32 entries, filled at init (`+0xf4` object field; stride 0x20 values → priority/routing level per source) |
| `+0x2C4` | w | control (cleared at init) |

Evidence: mask routine `FUN_0x3404` stores `~mask` to `0xCEFF0014` and `0x80F00014`, and to
`0x80F00010`; init `FUN_0x1e7c4` writes `0xFFFFFFFF` to `base+0x14`, `0` to `base+0x00` and
`base+0x2B8`, loops 32× over `base+0xf4…`, and reads/writes `base+0x30`. Enable/disable for one
IRQ: `FUN_0x1e84c` builds `1<<(irq&0x1f)` and OR/ANDs the per-bank enable word.

## Related

- **System timer**: the kernel reads a **64-bit free-running counter at `0xC0000098`**
  (`ldrd r0,r1,[0xC0000098]`) for timekeeping (in the ASIC core block, adjacent to the RTC at
  `+0x97`, see `docs/05`). (The boot ROM's `0xF9FFFFFC` is a separate early/boot counter.)
- **Secondary aggregators (bridged)**: the **CPLD interrupt** block (`0x01F14000` `+0x08`/`+0x2C`,
  button + board sources, `docs/09`) and the **expansion-bus** interrupt (`expbus_event_arm_
  interrupt MASK_REG`, 8-bit, via `iod_cpld`) hang off the main IC as nested controllers.

## For the OpenBMC port

Two interrupt-controller DT nodes (or one driver with two 32-bit banks):
`interrupt-controller@ceff0000` and `interrupt-controller@80f00000`, `#interrupt-cells = <1>`,
64 lines total. No upstream `hpe,gxe-intc` — a small custom irqchip is needed. Peripheral IRQ
numbers (I2C, GXELAN MAC0/MAC1, CHIF, KCS, CPLD) still need to be read from the kernel's
IODevice/`RequestInterrupt` registrations to fill `interrupts = <N>` per device.

## Open

- Exact per-source line numbers for each peripheral (I2C, MAC, CHIF, KCS, timer) — in the
  kernel's `RequestInterrupt`/IODevice tables.
- Confirm `+0x0C`/`+0x10`/`+0x30` precise roles (pending vs raw-status vs ack) by disassembling
  the IC poll function pointer registered on each block.

---

## Per-device IRQ line numbers — status (static extraction is not possible)

Attempted to map each peripheral to its IC line (`interrupts = <N>`). **The per-device line
numbers are not static constants in the firmware.** INTEGRITY binds device → IRQ line at
**runtime** through its interrupt-object framework:

- Devices register via interrupt **objects** (keys), mapped to hardware vectors by
  `interruptmapping.c`; the map is built with **function pointers**, not immediate
  `(device, line)` literals — there are **0 direct `bl` callers** with immediate IRQ args, and
  the IRQ number reaches the HW-enable path as a *parameter* (bounds-checked `< 0x20` per bank).
- Drivers request interrupts by **resource/IODevice name** (e.g. I2C `RequestResource("i2c%d")`,
  the ethernet IODevice), and the kernel resolves the line from the IODevice's interrupt field —
  configured in BSP data, not emitted as a discoverable `{base, irq}` table.

**To obtain the actual lines**, use one of: (a) **runtime/JTAG** observation on hardware (read
the IC enable regs `0xCEFF0014`/`0x80F00014` after each driver starts); (b) **dynamic trace**
of the interrupt-object construction; (c) **HP schematics** for the Gromit-XE interrupt routing.

What *is* fixed and usable now: **64 lines, IC0 `0xCEFF0000` (0–31) + IC1 `0x80F00000` (32–63)**,
enable at `+0x14`. For a first OpenBMC bring-up, wire the irqchip and discover per-device lines
empirically (they are stable per board/ASIC rev).

---

## Per-device IRQ map — RECOVERED (corrects the note above)

The earlier "not statically extractable" conclusion was **wrong** — it was looking in the app
image. The **type-2 kernel BSP** (`decompiled/kernel_type2.bin`) holds the full **IODevice
registry** (`0x1a8b7`–`0x1d7d0`: `IODI2C0-7`, `IODMAC0/1`, `IODLEGACYKCS`, `IODLPCCOM0-2`,
`IODPECI`, `IODMCTP`, …; BSP path `C:\iLO\iLOKernel\firmware\restricted\bsp\…`). Each `iod*_init`
registers its device with the **IRQ line as an immediate** — extracted below.

Lines 0–31 → **IC0 `0xCEFF0000`**; lines 32–63 → **IC1 `0x80F00000`**.

| IRQ | Device (IOD) | | IRQ | Device (IOD) |
|-----|--------------|-|-----|--------------|
| 1  | IODEXPBUS (expansion bus) | | 40–42 | IODLPCCOM0-2 (SOL/host serial) |
| 7  | IODTIMER1 (timer) | | 43 | IODPCICOM |
| 8–15 | **IODI2C0-7** (8 I2C engines) | | 44 | **IODLEGACYKCS** (IPMI KCS) |
| 16 | **IODMAC0** (dedicated NIC) | | 46 | IODPCIKCS |
| 17 | **IODMAC1** (sideband NIC) | | 47 | IODLEGACYBT |
| 22 | IODUSB | | 49 | IODSIGP |
| 26–28 | IODSPI0-2 | | 51–52 | **IODUHCIHC** (virtual USB) |
| 30 | IODSDCTL | | 55 | IODFAN |
| 31 | IODPWRMGR | | 56 | IODPCIEERROR |
| 36 | IODWALLACE | | 57 | **IODIRC** (remote console/KVM) |
| 37 | IODIOPFAULT | | 58 | IODPCIBT |
| 38 | IODSES (enclosure) | | 59 | IODHOSTI2C |
| 32? | IODHOSTFAULT (tentative) | | 60 | **IODPECI** (CPU PECI) |
| | | | 62 | **IODMCTP** |
| | | | 63 | IODSNA |

(`IODEXPBUS`/`IODHOSTFAULT`/`IODSES` init pass extra immediates; their IRQ is the distinct line
shown. `IODNETCFG`/`IODNLOOP` are virtual — no hardware line.)

### Devicetree `interrupts` — now fillable

e.g. `i2c@1f10100 { interrupts = <8>; … <15>; }`, `ethernet@c0004000 { interrupts = <16>; }`,
`ethernet@c0005000 { interrupts = <17>; }`, KCS `<44>`, UHCI `<51>`, PECI `<60>`, timer `<7>`.
Lines <32 use IC0, ≥32 use IC1.

### New devices this registry revealed

Beyond what was mapped: **IODMCTP** (MCTP), **IODPECI** (CPU PECI, also a CPLD-block path),
**IODSES** (storage-enclosure), **IODSIGP**, **IODLPCCOM0-2** (host serial/SOL over LPC),
**IODHOSTI2C** (host-side I2C), **IODPCIBT/PCICOM/PCIKCS/PCIEERROR** (host-interface channels),
**IODSDCTL**, **IODWALLACE** (Gromit-XE companion block), **IODSNA**, **IODEXPBUS**.

## `0xD1000000` (64 KB) — timer / Wallace companion block

The window the chase started from: byte registers at `+0x00`(bit7 polled in the timer tick),
`+0x02/03/04(word)/06/09/0d/20/24/34/38`. Accessed by the low-level **timer tick handler**
(`0x1acc4`, which also reads the `0xC0000098` 64-bit counter) and device-init `0x76e0`. Sits
among `IODTIMER1`/`Timer0 Exception Handler`/`IODWALLACE` — it is the **timer / "Wallace"
companion register block** (exact IOD-name binding TBD). Also present here: `unknown memory
configuration`, `Video memory offset … not at top of memory`, `ECC limit` — DRAM/video-memory
geometry config.
