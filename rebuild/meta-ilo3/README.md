# meta-ilo3 — HP iLO 3 (Gromit-XE) OpenBMC layer (DRAFT)

A from-scratch OpenBMC machine layer for the **HP iLO 3** BMC (ProLiant Gen7),
reconstructed **entirely by static reverse-engineering** of iLO 3 firmware 1.94
(`firmware/staging/ilo3_194.bin`). The centrepiece is a hardware-description
**devicetree** built from the findings in `../../docs/02`–`15`.

> **Status: hardware description, not a working build.** The `compatible =
> "hpe,gxe-*"` blocks have **no upstream Linux/U-Boot drivers** — those are the
> net-new work. The DT compiles cleanly (`dtc`), and every address/IRQ is sourced
> from the firmware, but sizes/bit-fields should be confirmed on real hardware.

## SoC identity (docs/02)

- **CPU:** ARM926EJ-S — **ARMv5TEJ**, little-endian. Ghidra `ARM:LE:32:v5`.
- **ASIC:** HP **Gromit-XE** ("GXE"), variant PN **438893-503**. RTOS (original
  fw): Green Hills **INTEGRITY**.
- Device regions are **identity-mapped** (VA = PA; the kernel runs IRQ context at
  FCSE PID 0) — so the DT `reg` values are physical. See `docs/10`.

## What the devicetree maps

`recipes-kernel/linux/linux-ilo3/ilo3-gxe.dts` (validated with `dtc 1.7.0`):

| Block | Base | IRQ | doc |
|-------|------|-----|-----|
| Interrupt controller (2×32 banks) | `0xCEFF0000` + `0x80F00000` | — | 12 |
| UART (custom 8-bit) | `0xC00000F0` | polled | 05 |
| ~~RTC~~ (none — software clock + SNTP) | — | — | 18 |
| Crypto engine | `0xC000000C` | — | 05 |
| 64-bit timer | `0xC0000098` | 7 | 05,12 |
| SPI-NOR boot flash (SMIF) | `0xC0000200` | 26-28 | 15 |
| Syscon (GXELAN reset @+0xC0) | `0xC0000000` | — | 05,11 |
| Ethernet MAC0 (dedicated) + MDIO | `0xC0004000` | 16 | 11 |
| Ethernet MAC1 (sideband/NC-SI) | `0xC0005000` | 17 | 11 |
| MAC DMA | `0xA1000000` | — | 13 |
| Fan/PWM | `0x01F00000` | 55 | 05,12 |
| Host interface (CHIF/VAS/power/SMBIOS) | `0x01F02000` (+`0x01F03000`) | — | 06 |
| IPMI KCS + watchdog | `0x01F0D400` | 44 | 05 |
| Host System-ROM SPI (romchf) | `0x01F10000` | — | 15 |
| KVM video capture | `0x01F100C0` | 57 | 14 |
| I2C/SMBus ×8 engines | `0x01F10100`+N·0x10 | 8-15 | 05 |
| CPLD / PWRMICRO (UID LED, power, PSU, PECI) | `0x01F14000` | 31 | 08,09 |
| Timer/"Wallace" companion | `0xD1000000` | 36 | 12 |

Reserved memory: KVM framebuffers `0xB0000000` (64 MB), CHIF shared-mem
`0x40000000` (docs/06,14). Full physical map: `docs/13`. Full IRQ list
(0-63, incl. PECI 60, MCTP 62, LPC-COM 40-42, …): `docs/12`.

## Build it / check it

```sh
dtc -I dts -O dtb -o /tmp/ilo3.dtb \
    recipes-kernel/linux/linux-ilo3/ilo3-gxe.dts     # validate the DT

# As a Yocto layer (needs an OpenBMC tree + a linux-ilo3 kernel recipe):
bitbake-layers add-layer .../rebuild/meta-ilo3
MACHINE=ilo3-gxe bitbake obmc-phosphor-image           # will NOT boot yet
```

## Drivers written (RE-derived)

### Interrupt controller — `files/irq-gxe.c`
Root irqchip for the 2-bank, 64-line controller (docs/12). `IRQCHIP_DECLARE`
("hpe,gxe-intc"); installs the ARM root flow handler via `set_handle_irq`, a
64-line linear `irq_domain` (`interrupts = <line>`), `handle_fasteoi_irq`.
Registers: ENABLE `+0x10` / DISABLE `+0x14` (confirmed from fw mask fn 0x3404),
PENDING `+0x0C` read, EOI `+0x30` write-1-to-clear (best RE reading — confirm
exact pending/ack offsets on HW). of_iomap bank0=`0xCEFF0000`, bank1=`0x80F00000`.

### Timer — `files/timer-gxe.c` (clocksource + clockevent)
- **Clocksource + `sched_clock`** from the 64-bit free-running counter at
  `0xC0000098` (`+0x00`/`+0x04`; CONFIRMED via fw `ldrd`). `hpe,gxe-timer`.
- **Clockevent** (periodic + oneshot) on the Wallace timer `0xD1000000` IRQ 36
  (`hpe,gxe-wallace`): CTRL `+0x09`=`0x80` enable and STATUS `+0x00` bit7 pending
  are CONFIRMED; the **reload (`+0x04`) and ack are best-guess** — two `GXE_CE_*`
  #defines flagged "CONFIRM ON HARDWARE" (static RE could not pin them; the fw
  uses the counter for busy-wait delays and routes the OS tick through an
  abstracted clock object). One hardware test from working.
- Both nodes take rate from DT `clock-frequency` (**placeholder 1 MHz — fix on HW**).

### UART — `files/gxe-uart.c`

The console — the prerequisite for debugging everything else — is implemented:

- **earlycon** (`OF_EARLYCON_DECLARE("hpe,gxe-uart", …)`) → first output at boot.
- Full **tty serial driver** + console: polled TX (`uart_port_tx`), RX via an 8 ms
  poll timer (the UART has no TX/RX interrupt), registers `ttyGXE0`.
- Register map (docs/05): data `+0x00`, status `+0x05` (TXRDY bit5, RXRDY bit0),
  8-bit access. No baud/line-control regs (strap-set), so `set_termios` forces 8N1.

The bbappend drops it into `drivers/tty/serial/`, splices `SERIAL_GXE` into the
serial Kconfig/Makefile, and applies `gxe-uart.cfg`. The dts `chosen` enables
`earlycon` + `console=ttyGXE0,9600n8`.

> Written against the mainline `serial_core` API (6.x). **Not compile-tested here**
> (no kernel tree in this env) — build it against the target OpenBMC kernel. On
> kernels ≥ 6.10 the `port->lock` accesses may need the `uart_port_lock*()` wrappers.

### I2C/SMBus — `files/i2c-gxe.c`
Driver for the **8 Gromit-XE I2C engines** (docs/05,16). The ASIC runs a small
DMA/mailbox engine, not register bit-bang: fill a 16-byte buffer, set a descriptor
(len + cmd), kick, poll status. One `i2c_adapter` per engine (`hpe,gxe-i2c`,
`reg=<0x1f10100 0x300>`, IRQ 8-15). CONFIRMED: region bases (regs `0x1F10100+e*0x10`,
buffer `0x1F10180+e*0x10` [+0x200 for e6,7], descriptor `0x1F101E0+e*4`), 16-byte cap,
reset sequence, status `0x2`=done / bit2-3=error. **CONFIRM-ON-HARDWARE** (flagged
`GXE_I2C_CMD_*`): the command-byte direction encoding and that the descriptor-cmd write
triggers the transfer (the fw kicks via an INTEGRITY IODevice call not callable from
Linux). Poll-based for now; IRQ 8-15 can convert it to interrupt-driven.

### SPI-NOR boot flash — `files/spi-gxe-smif.c`
`spi-mem` controller for the iLO's own boot flash (the firmware image lives here).
The Gromit-XE SMIF (`gxekflsh`) is a register-file SPI master at `0xC0000200`
(docs/15,17), not an XIP window. All register offsets, opcodes and the 64-byte
data FIFO are CONFIRMED from fw disassembly: MODE `+0x02`, GO/BUSY `+0x04`,
OPCODE `+0x05` (unlock `0x55`/`0xAA` preamble), COUNT `+0x06`, ADDR `+0x08`, FIFO
`+0x40`; WREN `0x06` / RDSR `0x05` / PP `0x02` / block-erase `0xD8`. The spi-nor/mtd
core drives each op through `exec_op`; completion = poll GO bit1 then WIP (RDSR
bit0). **CONFIRM-ON-HARDWARE** (flagged `GXE_SMIF_GO_*`): the GO launch-value
encoding per op class and the bulk-read opcode (fw read path not isolated; modelled
via `0x03`+FIFO). DT `flash@0` = `jedec,spi-nor`; add `fixed-partitions` per docs/01,03.
> Written against the 6.x `spi-mem` API (`devm_spi_alloc_host`, ≥6.8). On older
> kernels use `devm_spi_alloc_master`. Not compile-tested here (no kernel tree).

### CPLD GPIO — `files/gpio-gxe-cpld.c`
Exposes the system CPLD / power-micro block `0x01F14000` (docs/08,09) as a flat
bit-addressable `gpio_chip` (`hpe,gxe-cpld-gpio`): **gpio N = byte `N>>3`, bit `N&7`**
(4096 lines over the 0x200 window), RMW get/set, sw-tracked direction (no DDR reg).
Chassis **UID LED = gpio 6** (`+0x00` bit6, CONFIRMED from uid.c) — wired to the
`gpio-leds` node (`gpios = <&cpld 6 0>`). Other discretes by number (docs/09): CPLD
status gpio 7, PSU enable gpio 192-195, PSU present gpio 200-203 (inferred).
**CONFIRM-ON-HARDWARE**: fw writes CPLD bits via the `iod_cpld` IODevice (locking
wrapper over this MMIO) — RMW should drive pins, may need a HW mutex. UID button
(CPLD IRQ, source `+0x2C`) not yet a gpio-irqchip — future work.

### PECI — `files/peci-gxe.c`
Linux PECI **controller** for the native PECI engine (CPU DTS temp, RdPkgConfig,
RdPCIConfig) — docs/26. Engine is in the 0x01F00000 window at **`0x01F00400`**
(CMD +0x00, STATUS +0x04, bit-timing +0x08, enable +0x0C=0x80), IRQ 60; bit-timing
chosen from rev strap `0x01F100A0`; multi-node target MUX = CPLD `0x01F14068` (XNC
only). All four engine regs + init CONFIRMED. The PECI core issues standard commands
via `->xfer`; the driver runs each through a descriptor-DMA model. **CONFIRM-ON-HW**:
the descriptor byte layout + response offset (IODPECI abstraction). Feeds `dbus-sensors`
`peci-cputemp`. (NOT the earlier `0x01F14068` — that's only the MUX.)

### Fan scan-chain hwmon — `files/fan-gxe.c`
hwmon driver for the **CPLD PWM/tach serial scan-chain** fan controller (docs/25,27) —
NOT a flat PWM/tach register array (no aspeed-pwm-tacho analogue). Fan block
`0x01F00200`: CTRL +0x0C (mode), 8-byte DATA +0x10-0x17 (FET/PWM bits), TACH_CTRL
+0x18 (arm=0xFA), TACH_CFG +0x1C; latch strobe = CPLD `0x01F1400B` bit1 (via
`hpe,cpld` phandle). RESYNC sequence + registers CONFIRMED. Exposes `pwmN`/`fanN_input`
to **phosphor-pid-control**. **CONFIRM-ON-HW**: PWM duty-byte vs software-FET-toggle
semantics, per-fan chain bit position (DT-described), tach result reg + raw→RPM scale.

### GXELAN Ethernet MAC + MDIO — `files/gxe-gmac.c`
Netdev + MDIO driver for the two GXELAN MACs (docs/11): MAC0 `0xC0004000` (dedicated,
own PHY on MDIO) + MAC1 `0xC0005000` (sideband/NC-SI). **CONFIRMED + implemented**: the
full **MDIO** protocol (`0xC0004080` cmd: reg[4:0]/phy[20:16]/bit8=read/bit9=trigger,
`0xC0004084` data), MAC **reset** (core `0xC00000C0`: MAC0=bit0x04/MAC1=bit0x80), station
address (+0x18/+0x1C/+0x20), **multicast hash** (+0x24/+0x28, CRC-32 poly 0x04C11DB6),
control reg +0x14. **NOT recoverable from static RE** (flagged, scaffolded): the TX/RX DMA
**ring/descriptor format** — the fw manages rings as INTEGRITY DMA descriptor objects above
the hardware registers; `ndo_start_xmit`/NAPI are a marked scaffold pending hardware bring-up.
MAC1 = NC-SI (`CONFIG_NET_NCSI`, no PHY). MAC addr from NVRAM/netcfg (DT/random fallback).

## Net-new work before this boots

1. **U-Boot / SoC bring-up** for Gromit-XE (DDR init is in the raw boot ROM at
   flash `0x7f0000` — `docs/03` — as a reference).
2. ~~Linux **irqchip**~~ (done, `files/irq-gxe.c`).
3. ~~**GXELAN MAC** + MDIO~~ (done, `gxe-gmac.c`: MDIO/reset/addr/hash CONFIRMED; **TX/RX DMA ring format = HW bring-up**, scaffolded). NC-SI stack for MAC1. (Fans done, `fan-gxe.c`.)
4. ~~GXE UART~~, ~~timer~~, ~~I2C~~, ~~SMIF SPI-NOR~~, ~~CPLD-gpio~~ (done; HW-confirm items flagged) + **video** driver. (No RTC driver — software clock + SNTP, docs/18.)
5. ~~GXE **PECI**~~ (done, `files/peci-gxe.c`; descriptor layout = HW-confirm) → feeds dbus-sensors peci-cputemp.
5. **CHIF/VAS/vUSB host-interface** server (the host-facing PCI functions) — the  **→ design spec: docs/28** (both doorbells resolved; kernel CHIF driver + channel→service routing).
   hardest part (vKVM/vMedia). Linux host side already exists (`drivers/misc/hpilo.c`).
6. Wire OpenBMC services: `phosphor-ipmi-kcs` (KCS@`0x01F0D400`), power/PSU,
   sensors (I2C + PECI), `obmc-ikvm` (video@`0x01F100C0`).

## Caveats

- DRAM size is a placeholder (32 MB window seen; confirm on HW).
- A few blocks (Wallace, SD-control, SNA, some host-interface channels) are only
  partially understood.
- Do **not** redistribute HP's original firmware; this layer contains only
  RE-derived hardware description.
