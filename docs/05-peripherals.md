# iLO 3 Gromit-XE peripheral map (`0xC0000000`) — for the devicetree

Date: 2026-10-20. Method: static RE of the boot ROM (`decompiled/bootrom_7f0000.bin`,
`ARM:LE:32:v5` @ `0x7f0000`) + MMIO constant/offset harvest across boot ROM and main OS
(`decompiled/os_main_base0.bin`). All CONFIRMED entries are read directly from disassembly.

## MMIO regions found

| Region       | Role                                   | Confidence |
|--------------|----------------------------------------|------------|
| `0xC0000000` | Gromit-XE primary register block (sys-ctrl, crypto, RTC, UART) | **confirmed** |
| `0xCEFF0000` | **interrupt controller IC0** (IC1 at `0x80F00000`) — 64 IRQs, see docs/12 | confirmed |
| `~0xFA000000`| free-running timer/counter (`0xF9FFFFFC` read for elapsed time) | **confirmed** (role inferred) |
| OS driver bases (below) | I2C / GPIO / MAC / SPI etc. | candidates only |

## `0xC0000000` block — confirmed register offsets

Verified from boot-ROM disassembly:

| Offset | Access | Function | Evidence |
|--------|--------|----------|----------|
| `+0x00`–`0x08` | ldr/str | system control (reset/config) | early init |
| `+0x0C` | ldr/str | **crypto engine** ctrl/status | only touched by the decrypt/hash/signature path (`Starting decrypt`→`Signature valid`); matches `Corsec_asic` |
| `+0x20,0x24,0x44,0x4C,0x64` | ldr/str | system / DDR / clock config | boot-time init |
| `+0x97` | strb | timer **latch/ack** byte (NOT RTC) — see **`docs/18`** | written 0 by tick-handler teardown |
| `+0x98` | ldrd | **64-bit timer counter** (timebase; the "time field") — docs/12 | kernel get-time `0x3468` |
| `+0xA0` | ldr | **chip-ID / revision** reg (NOT RTC) — guarded vs ~`0x50000` | docs/18 |
| **`+0xF0`** | strb/ldrb | **UART data** (TX write / RX read, 8-bit) | `strb r2,[#0xf0]` in putchar; `ldrb r0,[#0xf0]` in getchar |
| **`+0xF5`** | ldrb | **UART status** | putchar polls `tst #0x20` (**bit5 = TX ready**); getchar tests `#0x01` (**bit0 = RX ready**) |

Console UART, decompiled:
```
putchar:  mov r1,#0xc0000000
          ldrb r0,[r1,#0xf5] ; tst r0,#0x20 ; beq .-   ; wait TX empty
          strb r2,[r1,#0xf0]                            ; write char
getchar:  ldrb r0,[r1,#0xf5] ; tst r0,#0x01            ; RX ready?
          ldrbne r0,[r1,#0xf0]                          ; read char
```
It is a **custom HP/Gromit-XE 8-bit UART**, not a 16550 (register layout and status bits
differ). For OpenBMC this needs a small custom earlycon/8250-variant or a new serial driver.

## OS driver base candidates (NOT yet labeled)

The OS loads peripheral bases indirectly from device structs, so static string-correlation
did not label them. Recurring round literal-pool constants in `0xC0000000`–`0xCFFFFFFF`
(refcount in parens) — treat as *leads*, confirm by per-driver RE:
`0xC0120000` (12), `0xC0200000`, `0xC1E00000` (4), `0xC3E00000` (4), `0xC7000000` (5),
`0xCB000000` (2), `0xCC000000` (12). (Some high "round" values such as `0xC0336400`,
`0xC0586300`, `0xCB007100`, `0xCCCC0000` recur in exactly 12 sites and are likely shared
*magic constants*, not MMIO — do not trust without disassembly.)

Known integrated blocks still needing a base (from firmware strings / drivers):
**8× I2C/SMBus engines** (`ASIC supports 8 I2C engines`), **GXELAN** on-die NIC/MAC,
**SMIF SPI-flash controller** (`smifpkt_*`), **GPIO** (straps `HSTRAP/SSTRAP`, LEDs, power
sequencing), interrupt controller, watchdog. These are the next RE targets.

## Devicetree skeleton (confirmed parts)

```dts
/ {
    #address-cells = <1>; #size-cells = <1>;
    cpus { cpu@0 { compatible = "arm,arm926ej-s"; device_type = "cpu"; reg = <0>; }; };

    memory@0 { device_type = "memory"; reg = <0x00000000 0x02000000>; }; /* 32 MB, see docs/04 */

    soc {
        compatible = "simple-bus";
        #address-cells = <1>; #size-cells = <1>; ranges;

        /* Gromit-XE primary register block */
        syscon@c0000000 {
            compatible = "hpe,gromit-xe-syscon", "syscon";
            reg = <0xc0000000 0x100>;
            /* +0x0c crypto, +0x20..0x64 clk/ddr cfg, +0x97 latch/+0xa0 chipID (no RTC, docs/18), +0xf0..0xf5 uart */
        };
        serial@c00000f0 {                       /* CONFIRMED console UART */
            compatible = "hpe,gromit-xe-uart";  /* custom 8-bit: data=+0x0, status=+0x5 */
            reg = <0xc00000f0 0x8>;             /* status bit5=TXRDY, bit0=RXRDY */
            /* earlycon candidate for first bringup */
        };
        /* NO rtc node — 0xC0000097/0xA0 are latch+chip-ID, not RTC. Software clock + SNTP. docs/18 */

        /* TODO confirm: timer ~0xfa000000, block @0xceff0000, I2C x8, MAC, SPI-flash, GPIO, intc */
    };
};
```

## Method notes / next steps

- MMIO was harvested by decoding ARM `mov rX,#0xC…` + following `ldr/str [rX,#off]` and
  `ldr rX,[pc,#…]` literal pools (base-independent). The boot ROM maps cleanly this way; OS
  drivers hide bases in structs and need per-driver tracing.
- **Next:** RE the I2C/SMBus driver (search `SMB*`/`I2C` string xrefs → accessor → base),
  the GXELAN MAC, and the SMIF SPI-flash controller to complete `reg`/`interrupts` for the DT.
- Confirm the `0xCEFF0000` block role and the timer base; find the interrupt-controller base
  from the IRQ/FIQ vector handlers (see `docs/03` exception strings).

---

# UPDATE — OS peripheral window `0x01F00000` + I2C/SMBus (docs 06-level detail)

The boot-ROM map above is the **physical, pre-MMU** view (ASIC core @ `0xC0000000`). The main
OS runs with the **MMU on** and accesses peripherals through a **virtual window at
`0x01F00000`–`0x02000000`** (top 1 MB of the 32 MB space; addresses lie *beyond* the 11 MB image
= genuine MMIO). Recovered by decoding driver base loads in `decompiled/os_main_base0.bin`.

## I2C / SMBus controller — RE'd (`src\i2cDriver.c`, `src\i2cServer.c`)

- **8 engines**, memory-mapped, **base `0x01F10100`** (engine 0), **stride `0x10`** (16 B/engine)
  → engines 0–7 at `0x01F10100, 0x01F10110, … 0x01F10170`. (Confirmed: globals `@0xe33c0`=
  `0x01f10100`, `@0xe33c4`=`0x01f10108`; access `base + engine<<4`.)
- **Per-engine register layout** (from `i2c_validate`/reset routine disasm, 8-bit regs):

  | Off | Access | Meaning (inferred) |
  |-----|--------|--------------------|
  | `+0x00` | ldrb | status — bit0, bit1, **bit4 `0x10`** (enabled/active), **bit5 `0x20`** |
  | `+0x01` | ldrb/strb | control / error — error bits mask **`0xE0`**; write 0 to clear |
  | `+0x04` | strb | data/config (reset writes `0x80`) |
  | `+0x08` | strb | **command** (reset sequence writes `0xB0` then `0x30`) |

- **Interrupts:** per-engine (`I2C%d_intTask`, `i2c_enableInt` → kernel `RequestInterrupt`).
- **Software layer:** INTEGRITY IODevice named `IODI2C`; the physical mapping is held in the
  **kernel** IODevice registry — kernel code lives *outside* this image (driver calls branch to
  `0x018xxxxx`, beyond the 11 MB app image), so the kernel is a separate INTEGRITY segment.
- Adjacent **SMIF SPI-flash controller** at `0x01F10000` (same 4 KB page as the I2C block).

## OS peripheral-window block map (virtual `0x01F00000`)

| Block (VA) | Peripheral (from driver strings) |
|------------|----------------------------------|
| `0x01F00000` | fan / PWM / thermal control (`fan`, `TEMPERATURES`, `PWM %u`) |
| `0x01F02000` | **host/PCI interface** (CHIF/VAS/host-power/SMBIOS) — see docs/06 |
| `0x01F06000` | (unlabeled) |
| `0x01F07000` | temperature sensor driver (`src\drvtemp.c`) |
| `0x01F0D400` | **KCS (IPMI) + Watchdog** (`KCS`, `WDT Refresh`) |
| `0x01F10000` | **SMIF SPI-flash controller** (`smifpkt_*`, `smif 0x0087`) |
| `0x01F10100` | **I2C/SMBus — 8 engines** (above) |
| `0x01F13000` | (unlabeled) |
| `0x01F14000` | **power manager + CPLD interface** (`PWRMGR_API_*`, PWRMICRO; CPLD regs: UID LED/button, inputs — see docs/08) |

## Physical vs virtual (devicetree caveat)

`0x01F00000` bases are **OS virtual** (MMU on). OpenBMC/Linux needs **physical** bases. Two
known anchors: boot-ROM physical ASIC @ `0xC0000000` (UART/RTC/crypto, confirmed), OS virtual
peripheral window @ `0x01F00000`. The VA→PA relation for the OS window is **not yet traced** —
it requires the kernel's MMU page-table / `IODevice` physical-base config (kernel segment, not in
this image). **Next:** find the MMU setup (TTBR0 / `mcr p15,0,…,c2`) or the IODevice registration
to resolve `0x01F10100` (and siblings) to physical; only then are these DT `reg` values final.

## Devicetree — I2C node (virtual base; mark physical TBD)

```dts
/* OS-virtual base shown; replace with physical once VA→PA is traced */
i2c: i2c@1f10100 {
    compatible = "hpe,gromit-xe-i2c";
    reg = <0x01f10100 0x80>;     /* 8 engines x 0x10 */
    #address-cells = <1>; #size-cells = <0>;
    /* engine N regs: +0 status, +1 ctrl/err(0xE0), +4 data, +8 cmd; per-engine IRQ */
};
spi-flash@1f10000 { compatible = "hpe,gromit-xe-smif"; reg = <0x01f10000 0x100>; };
```
