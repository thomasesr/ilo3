# CLAUDE.md

Guidance for Claude Code working in this repository.

## Project Objective

Analyze the firmware/software for **HP iLO 3** (Integrated Lights-Out 3),
a product that is no longer supported by HP/HPE. The goal is to identify hardware specific stuff to allow to port OpenBMC and add support for OpenBMC on G7 Proliant servers.
get info from the shipped binaries.

Work in scope:
- analyse iLO 3 firmware images / binaries into readable source (C/assembly).
- Document the binary layout, architecture, and runtime behavior.
- Identify hardware specific entrys and port OpenBMC.
- Produce artifacts that allow building or flashing updated alternative OpenBMC firmware.

## Background: What iLO 3 Is

HP iLO (Integrated Lights-Out) is an out-of-band management controller — a **BMC**
(Baseboard Management Controller) embedded on HP ProLiant server motherboards. It runs
independently of the host OS and CPU, powered by standby power, giving remote management
even when the server is off.

- **iLO 3** shipped on HP ProLiant **Gen7 (G7)** servers (~2010–2012 era).
- Provides: remote console (virtual KVM), virtual media, power control, hardware health
  monitoring, IPMI, SNMP, web UI (HTTPS), SSH/CLI, and the XML scripting interface (RIBCL).
- SoC: typically an **ARM**-based management processor. iLO 3 commonly uses a GLM/SoC with
  an ARM core running a proprietary RTOS/embedded Linux-like stack. Confirm exact core from
  the binary before assuming instruction set.
- Firmware distributed by HP as signed image bundles (`.bin` / scexe / flash components).
  Later firmware enforces image signing — relevant when updating/re-flashing.

### Why Analyse
iLO 3 is end-of-life. No new firmware, no security patches. Modern browsers/TLS stacks
reject its old crypto; management features break. Recovering source enables patching these
issues on hardware still in service.

## Background: What OpenBMC Is

**OpenBMC** is a Linux Foundation project building an open-source firmware stack for BMCs —
the direct replacement target for iLO 3 on this hardware. It is the "port to" side of this
project; iLO 3 analysis is the "port from" side.

Stack shape:
- **Yocto/OpenEmbedded build system.** The repo is a set of BitBake layers (`meta-*`), not a
  monolithic source tree. A build produces a flashable image (U-Boot + Linux kernel + rootfs).
- **Linux kernel** on the BMC SoC (ARM, typically ASPEED AST2400/2500/2600 or Nuvoton NPCM;
  HPE's own SoC family is **GXP**). Board support = a devicetree + machine `.conf`.
- **D-Bus as the system bus.** Every service (sensors, power, inventory, LEDs) publishes and
  consumes typed D-Bus interfaces defined by **phosphor-dbus-interfaces**. This is the
  integration contract — porting a feature means exposing it on the right D-Bus interface.
- **phosphor-\*** userspace daemons implement the BMC functions: `phosphor-hwmon` /
  `dbus-sensors` (sensors), `x86-power-control` (power sequencing), `phosphor-state-manager`
  (host/chassis/BMC state), `phosphor-fan-presence` / `phosphor-pid-control` (fans),
  `phosphor-ipmi-host` + `phosphor-ipmi-kcs` (IPMI), `phosphor-logging` (event log).
- **bmcweb** — HTTPS server exposing a **Redfish** API (the modern replacement for iLO's RIBCL
  XML interface) plus KVM/virtual-media endpoints. Modern TLS, actively maintained — this is
  what fixes the "browsers reject iLO 3 crypto" problem.
- **entity-manager** — runtime hardware description from JSON config, replacing hardcoded
  board tables.

### Why OpenBMC for iLO 3

iLO 3 is EOL with no patches. OpenBMC is maintained, has current TLS/crypto, and already
carries an HPE layer. The port is: identify what in iLO 3 is *hardware* (SoC registers, clock
and pinmux, LPC/eSPI to host, PECI, I2C/SMBus sensor topology, power sequencing GPIOs, flash
layout) versus what is *HP application software* (web UI, licensing, RIBCL) — then re-express
the hardware half as a devicetree + machine layer and let OpenBMC supply the software half.

### Upstream HPE support already in-tree (`openbmc/meta-hpe/`)

Relevant prior art — HPE already upstreams a BMC SoC layer:

- `meta-gxp/` — BSP for the **HPE GXP** SoC. `conf/machine/include/gxp.inc` declares:
  `UBOOT_ARCH = arm`, `SOC_FAMILY = gxp-b1`, `DEFAULTTUNE = armv6-novfp`
  (`arch-armv6.inc`), `UBOOT_ENTRYPOINT = UBOOT_LOADADDRESS = 0x40100000`.
  U-Boot fork: `github.com/HewlettPackard/gxp-uboot`, branch `v2020.10-gxp-openbmc`.
  Mainline Linux carries GXP support under `arch/arm/mach-hpe/` + `gxp.dtsi`.
- `meta-common/` — shared HPE recipes (`recipes-hpe/host`, `vehci`, IPMI, packagegroups).
- Machine layers: `meta-dl360-g11`, `meta-dl385-g11`, `meta-rl300-g11`, `meta-dl360poc`
  (Gen11 servers). Each machine `.conf` is tiny — `KERNEL_DEVICETREE = gxp.dtb` plus three
  `require` lines. **`meta-dl360poc` is the best template** for a new HPE machine port.
- `conf/machine/include/hpe.inc` sets the service wiring: `x86-power-control` for host and
  chassis state, `phosphor-ipmi-kcs` for host IPMI, `packagegroup-hpe-apps` for
  chassis/fan/flash/system management.

**SoC resolved (`docs/02-soc-id.md`):** iLO 3's ASIC is HP's **Gromit-XE (`GXE`)** —
distinct silicon from GXP (iLO 4/5 era). So `meta-gxp` is a **structural template only**, not
reusable SoC support: a port needs a new `meta-ilo3` layer + from-scratch Gromit-XE devicetree
(mainline Linux has no Gromit-XE support). CPU core = **ARM926EJ-S / ARMv5TEJ** → OpenBMC tune
**armv5te** (older than meta-gxp's armv6). Ghidra: `ARM:LE:32:v5`.

## Status / Progress

- **Env**: Ubuntu 24.04 container, apt + passwordless sudo. Installed: Ghidra 12.1.4 (`/opt/ghidra`,
  `ghidra` cmd; headless `analyzeHeadless` verified), OpenJDK 21, binwalk 2.3.3, jefferson, 7z,
  unsquashfs, rpm2cpio. GUI needs X11 — plan around **headless** Ghidra.
- **Firmware acquired** (`firmware/`, SHA256 verified): iLO 3 FW **1.94** as `.rpm` and `.scexe`.
- **RPM unpacked** → flash image `ilo3_194.bin` (8.4 MB) staged at `firmware/staging/ilo3_194.bin`.
  Format identified, header + section table partly decoded, entropy-mapped. See `docs/01-rpm-unpack.md`.
- **OpenBMC cloned** → `openbmc/` (shallow, commit `0d89cc3`, 2026-10-20, ~95 MB). `meta-hpe`
  present with `meta-gxp` BSP (ARMv6/`gxp-b1`) + Gen11 machine layers.
- **Payload = compressed, not encrypted** (HP LZSS, 0x1000 window; `tools/ilo_lzss.py`).
- **SoC identified: HP "Gromit-XE" (`GXE`) ASIC**, Green Hills INTEGRITY RTOS. See `docs/02-soc-id.md`.
- **CPU core: ARM926EJ-S (ARMv5TEJ)** → Ghidra `ARM:LE:32:v5`. GXE variant PN **438893-503**.
- **ARMv5 CONFIRMED from real code**: boot ROM raw/XIP at flash `0x7f0000` disassembles clean
  as `ARM:LE:32:v5` (65 funcs). LZSS sections = resources, NOT code. See `docs/03-bootloader.md`.
- **HW found**: MMIO base `0xC0000000` (Gromit-XE regs), DDR3 init, clock straps, ARM CP15
  exception handlers; boot ROM does sig-check + decrypt + decompress (iLO3–7 lineage).
- **Main OS LOCATED**: the 12 MB `0x440` image IS the INTEGRITY ARM926 monolith (17,152 funcs);
  code is byte-phase-1 shifted (drop 1 byte to align). **RAM base `0x00000000`, 32 MB space.**
  Clean v5 disasm, BL targets resolve, APCS frame ptrs, `swi` syscalls. See `docs/04-main-os.md`.
- **Peripherals mapped** (`docs/05-peripherals.md`): `0xC0000000` block = Gromit-XE sysctl +
  **crypto `+0x0C`** + ~~RTC~~(none-see below) + **console UART `+0xF0`/status`+0xF5`** (custom 8-bit,
  TXRDY=bit5, RXRDY=bit0). Timer ~`0xFA000000`; 2nd block `0xCEFF0000`. DT skeleton drafted.
- **I2C/SMBus RE'd**: 8 engines @ OS-virtual **`0x01F10100`**, stride `0x10`; regs +0 status,
  +1 ctrl/err(0xE0), +4 data, +8 cmd. OS peripheral window **`0x01F00000`** maps SMIF-SPI
  (`0x1F10000`), KCS+WDT (`0x1F0D400`), fan/PWM (`0x1F00000`), pwrmgr (`0x1F14000`), temp
  (`0x1F07000`). See `docs/05-peripherals.md`. Bases are VIRTUAL (MMU on).
- **Host PCI interface mapped** (`docs/06-pci-interface.md`): Gromit-XE = multifunction PCI
  endpoint `103c:3306`(.0 VAS/instr) `103c:3307`(.2 CHIF msg) `103c:3300`(.4 vUSB=**UHCI**).
  BMC side = host-iface block `0x01F02000` (CHIF BAR `+0x50`, VAS `+0x54`, hostpwr `+0x46`,
  SMBIOS `+0xA0`) + CHIF shared-mem transport (`src\chif.c`, sawbase, phys window ~`0x40000000`).
  **Verified vs live host `lspci -vvv`**: CHIF shared-mem = `.2` BAR2 (1 MB @f1e00000),
  option ROM 64 K (`.2`), subsystem names "iLO3 GXE". Linux host drivers: `hpilo`(.2),
  `uhci_hcd`(.4). BMC-side CHIF/VAS/vUSB server is net-new for OpenBMC.
- **CHIF protocol decoded** (`docs/07-chif-protocol.md`): doorbell/status `0x01F03004`
  (bit2=msg-pending), CHIF ctrl `0x01F020D0/D4` (enable `0x04000000`), BAR reg `0x01F02050`.
  10 channel descriptors (0x4C B @ `0x62A7C`, `+0x28`=chan id, 0xffff=free); 32 shared-mem
  CCBs (0x80 B, `+0x04`=msg offset, bit31 ownership @`+0x10`/`+0x28`); msg header chan-id @+0x4006.
- **Also open**: BMC→host CHIF doorbell write; remaining CHIF descriptor/CCB ring fields.
- **Next (port side)**: confirm iLO 3's actual ARM core / SoC ID from the image, then decide
  whether `meta-gxp` is reusable or only a structural template for a new `meta-ilo3` layer.

- **UID/LED control** (`docs/08-uid-led.md`): system/chassis **UID LED = CPLD** (MMIO
  `0x01F14000` + `iod_cpld`; UID button = CPLD IRQ), NOT I2C. Health LEDs = ASIC reg banks
  (`0x01F00200`/`0x01F14000`, SDR RMW). Only **per-drive-bay UID LEDs = I2C**(+SGPIO) to backplane.

- **CPLD/PWRMICRO block `0x01F14000` mapped** (`docs/09-cpld-register-map.md`, ~0x200 B):
  `+0x00` CPLD status/**UID**(bit6), `+0x01/02` CPLD ver, `+0x04-0x0C` host-power, `+0x08/0x2C`
  CPLD int/button, `+0x0B` fan, `+0x18-0x1A` PSU, `+0x68` PECI, `+0x1F5` pwrmgr. Also via `iod_cpld`.

- **PSU telemetry source** (`docs/19`): wattage/Vin/Iin/VA/capacity/**health** come from the
  **power supplies over I2C** (smart PSUs, PANDEMONIUM/"panda" family: `pwrsupply_panda_read/write`,
  `pwrburn_total_pwrsupply_input_power_get`), + per-bay **FRU EEPROM** (model/PN/serial), mediated by
  PWRMICRO/"ELECTRA" on I2C buses 0-5. CPLD `0x01F14000` gives ONLY presence(`+0x19`)/enable
  (`+0x18` EXPBUS_PS_ENABLE)/bays(`+0x1A`). OpenBMC: phosphor-power/psu-manager + dbus-sensors PMBus
  on the i2c-gxe adapter, FRU via entity-manager; presence/enable via CPLD-gpio.
- **Panda PSU register map** (`docs/20`): PSU telemetry transport = `pwrsupply_panda_read(bus,addr,
  reg)` @`0x45912c` — HP-proprietary SMBus (write`[reg,cksum=0x100-reg-addr]`, read`[lo,hi,pec]`,
  sum-to-0 check, 3 retries), **NOT PMBus**. Registers: `0x00`=fwrev, `0x02`=status(bits[10:8]),
  `0x0A`=input current, `0x32`=peak power, `0x3A`=control(HEM/standby), measurement `0x0C/0x0E/0x12`
  + telemetry triple `0x2C/0x2E/0x30`. Identity=FRU EEPROM. Port: custom "panda" PSU daemon on
  i2c-gxe (buses 0-5) OR check for a std PMBus page; presence/enable via CPLD-gpio.
- **Host subsystem data paths mapped** (docs/22-25, parallel RE + disasm-verified):
  - **CPU + memory inventory** (`docs/22`): from host BIOS **SMBIOS over ROM-CHIF** (`romchf_service`),
    NOT PECI/SPD. `smbios_get_proc_info`@`0x8d100c` (type 4+7), `smbios_get_mem_info`@`0x8d143c`
    (type 17), fetcher `0x8cff4c` (opcode-3 by-type, VComClientSync). PECI/SPD = thermal/health only.
    Port: smbios-mdr (MDRv2) → Redfish Processors/Memory.
  - **Drive backplane** (`docs/23`): SEP over **I2C** `0x70-0x77` (8-bit `0xE0-0xEE`), ACK-probe
    discovery (`0x132160`). HP-proprietary frames (verified): UID-set `D8 00 07 06 <idx> <chk>`
    (`0x132548`), health/status read `D2 00 05 05 24` (`0x1322f8`), SGPIO `D2 00 06 05 23` (`0x13274c`,
    hdr `0x50`+cksum). Transport = `I2CService` RPC `0x136cd0`. Drive **model NOT from SEP** —
    host-sourced cache `[0xb6345]`. Port: i2c-gxe client + userspace SEP-frame shim → Led.Group +
    Inventory.Item.Drive. NOT SES/SAF-TE, NOT a HW SGPIO block.
  - **Temperatures** (`docs/25`): CPU via **PECI** (CPLD `0x01F14068`, IRQ60); board via **I2C LM75**
    (+PCA9544 mux/PCA9555) on the 8 engines; ASIC via CPLD. Sensor bus/addr = SDR runtime data, not
    code literals. Unified access-descriptor cache (CPLD/GPO/SChain/I2C/PECI...). docs/05 "0x1F07000
    temp block" = MMU aperture only, no code xref — corrected.
  - **Fans** (`docs/25`): **CPLD SERIAL PWM scan-chain** (open-drain FET bits clocked out + RESYNC),
    NOT flat PWM regs. 6 tach fans (FAN_1..6_TACH). Blocks `0x01F00000` + CPLD `0x01F14000`+0x0B.
    DT node = `hpe,gxe-fan-scanchain`; driver net-new (no aspeed-pwm-tacho analogue). Port:
    phosphor-pid-control + scan-chain hwmon.
  - **Host power** (`docs/24`): all disasm-CONFIRMED. vPWRBTN# = `0x01F02046` (press primitive
    `0x448910`, 1→hold→0; graceful vs force = hold duration); warm-reset `0x01F0005C`=0/`0x5D`=0x80;
    NMI `0x01F0006E` bit2; CSM cold-boot `0x01F000E7`=0xB2; power-state `0x01F02070` bit24=PGOOD/
    bit25=in-reset; phys button = CPLD `0x01F14008` bit6 + IRQ src bit `0x10`. Port: gpio-mmio shim
    over host-iface/CPLD → x86-power-control (warm-reset 2-byte seq + 0xB2 = net-new actions).
- **CHIF host-interface server spec'd** (`docs/28`, BMC-side design for OpenBMC): both doorbells
  now resolved — host→BMC = `0x01F03004` bit2 (msg pending), **BMC→host = write `(1<<channel)` to
  `0x01F020D4`** (`0x1532b8`; same reg as the `0x04000000` enable). Transport = 1MB shared-mem BAR
  (`0x01F02050`), 10 chan descriptors (`0x62A7C`)/32 CCBs (bit31 ownership), msg header chan-id
  @+0x4006. Dispatch `FUN_0x152fe0` (types 0x18/0x19/0x1A); routing = `chif_message_dispatch` `0x150cac`.
  **Channel ids host-assigned** (descriptor `+0x28`, dynamic); BMC reserves `0xFFFE`=echo/`0xFFFF`=free;
  per-service numbering = host hpilo convention. Port: net-new kernel CHIF driver (mirror of host
  `hpilo.c`) + userspace channel→service routing (IPMI/power/inventory reuse phosphor; vMedia/vKVM =
  largest net-new). **Ring = CCB ownership bit31, NO head/tail indices**; desc `+0x2A`=buf size; msg magic `"CHIF"`
  (type 0x18)/`"CHI5"`(0x19=ChifLite) @shmem+0x4000 +14B checksum; hdr len@+0/chan-id@+6; RX→per-
  service SW queue(depth 4). CHIF protocol fully mapped.
- **vUSB/vMedia bridge mapped** (`docs/29`): host-facing virtual USB = **software-emulated UHCI**
  (PCI `.4`; kernel IODevices IODUHCIHC`0x33`/IODUHCIWALKER`0x34`/IODUSB`0x16`, IRQ 51/52). BMC
  walker reads the host's UHCI frame-list/TD schedule over the CHIF shared window, executes
  transfers against virtual devices: **HID keyboard+mouse** (vKVM/IRC) + **USB mass storage**
  (BOT/SCSI → virtual floppy`.img`/CD`.iso`/USB-key/folder). Media source = IRC applet / scripted
  HTTP URL (`vm_url`/`vm_script`/`vm_stream`). **Corrects docs/06**: `eh_intf`=Event Handler
  (health events→host), NOT vMedia. Port = largest net-new: UHCI emulation + USB-gadget devices +
  nbd/virtual-media backend + obmc-ikvm (HID/video). Depends on CHIF server (docs/28).
- **DDR3 init FULLY MAPPED** (`docs/30`, boot ROM): memory-controller bring-up fn `0x7f198c` + a
  (reg,val) table @`0x7f1a0c`. Controller @`0xC0000000`: ctrl/status `+0x20` (write 0x3 start, read
  bit7=ready, write 0x2 post-cal), timing `+0x24/28/2c/30/34/40`, refresh `+0x48`, addr-mask `+0x18`,
  MEMCFG `+0x64` (from strap `0x801F00A4>>1`). Sequence: warm-check `+0x20` bit7 → 500-tick timer
  delay → write table phase1 → poll `+0x20` bit7 (PHY cal) → table phase2 (`+0x20`=0x2, `0x80200040`
  =0x400) → MEMCFG. Exact FW1.94 values captured. Port = U-Boot SPL dram_init. DRAM size = MEMCFG
  strap-derived (DT 32MB = placeholder).
- **Clock/strap config MAPPED** (`docs/30`, boot ROM): **strap/clock-select reg `0x801F00A4`** (bits
  [1:0]=mode, [27:22]=clock-sel; read board straps here, `tst #3` @`0x7f1af8`); **clock/PLL/reset
  block `0x80200000`** (`+0x88` PLL ctrl programmed from strap, `+0x40`=0x400, `+0x58` reset). DDR
  coupling: MEMCFG `0xC0000064` = min(cfg, clock>>1). Outputs = FRQUSB/FRQLPC/FRQMAC. New block
  `0x80200000` added to docs/13 (clock/PLL, != IC1 `0x80F00000`).
- **Secure boot / sig verify MAPPED** (`docs/31`, boot ROM): image verified by **RSA-2048 over a SHA-256 hash** (SHA-256 (HP envelope `Hash: sha256`; combined 256/512 const table @`0x7f4268`); `Starting hash`→`Starting
  decrypt`(RSA)→`Signature valid`/`unsigned - will not boot`). Public key = embedded **cert
  @flash `0x7e0000`** (HP root), sig block @`0x800000`; loader spans iLO3-7. **NVROVR** = NVRAM
  override countdown (recovery window, NOT a sig bypass). **Reflash implication**: boot ROM rejects
  unsigned FW; it lives in external SPI flash (`0x7f0000`) so is SPI-rewritable, BUT the gating
  open question = **is there an on-die mask ROM verifying it?** (trace reset vector). If flash boot
  ROM is top-of-chain, a patched/replaced boot ROM via SPI = owner route to OpenBMC.
- **Reset vector TRACED → root of trust RESOLVED** (`docs/31`/`docs/03`): boot ROM starts w/ ARM
  vector table (reset→`0x7f002c`), self-refs `0xFFFF0xxx` → **true base `0xFFFF0000`** (ARM926 high
  vectors); 8MB flash mapped `0xFF800000-0xFFFFFFFF` so flash`0x7f0000`=`0xFFFF0000`. **ARM926 resets
  DIRECTLY into external SPI flash** (reset handler `0xFFFF002c`: CP15/stack `0x20004000`/clock/DDR→
  verify→boot). **NO on-die mask ROM** (iLO3 predates HPE Silicon Root of Trust, a Gen10/iLO5
  feature) → **flash boot ROM = top of chain**. **REFLASH VERDICT: FEASIBLE** via SPI programmer —
  patch the sig-check branch or replace boot ROM w/ U-Boot SPL (DDR init docs/30) → unsigned OpenBMC
  boots. (HW-confirm: flash a patched ROM.)
- **Sig-check branch LOCATED** (`docs/31`): verify fn `0xFFFF3800` = SHA-256(image) → finalize to
  `0xA1001EC4` → memcmp 64B vs RSA-decrypted hash (r7) → **`0xFFFF38b8` `cmp r0,#0` / `0xFFFF38bc`
  `bne 0xFFFF39c4`** (bytes `1a000040`). = flash image **`ilo3_194.bin` offset `0x7f38bc`**
  (verified). **PATCH for unsigned boot**: `1a000040`(bne)→`e1a00000`(nop) → always "Signature
  valid" → unsigned OpenBMC boots. Boot ROM not self-checked. For owned EOL G7 HW (project goal).
- **Sig key/chain IDENTIFIED** (`docs/31`): **RSA-2048**, hash **SHA-256**, signing key `label_HPBBatch`
  ("HP Binary - Batch" leaf) in an HP "Signed File Fingerprint" envelope @`0x800000`. 3-level chain
  (leaf ← HP **Printing Device** Infrastructure CA ← self-signed Root CA; certs sha1-RSA-2048). HP
  reused their printer code-signing PKI. Legit signing needs HP's private key (unavailable) → patch path.
- **VA→PA RESOLVED** (`docs/10`): the **type-2 images (v0.8.42) ARE the INTEGRITY kernel+BSP**
  (not NIC data) — dense phase-1 ARM, MMU block (TTBR0 `MCR p15,c2` @type2+`0x0aff0c`, SCTLR/
  DACR/TLB), ARM926 **FCSE** (c13) for per-AddressSpace 0-32 MB windows. Kernel hits BOTH
  physical `0xC0000098` (RTC, = boot ROM) AND `0x01F14000` as literals → **device regions are
  identity-mapped (VA=PA)**. So ALL documented peripheral addresses ARE physical; DT `reg`s stand.
  Realigned kernel artifact: `decompiled/kernel_type2.bin`.

- **GXELAN MAC mapped** (`docs/11-gxelan-mac.md`): 2 NICs — **MAC0 `0xC0004000`** (dedicated/
  physical iLO port) + **MAC1 `0xC0005000`** (sideband/shared NC-SI w/ host LOM), 0x1000 apart.
  Shared **MDIO `0xC0004080`**(cmd)/`0xC0004084`(data); global reset `0xC00000C0` (16b). 32-desc
  DMA rings. Driver = `umac.c` in type-2 kernel. All physical (identity-mapped).

- **Interrupt controller mapped** (`docs/12`): 2 HW blocks, 64 IRQs — **IC0 `0xCEFF0000`**
  (irq 0-31) + **IC1 `0x80F00000`** (irq 32-63); enable reg `+0x14` (active-high), status `+0x0C`,
  per-source cfg `+0x100[32]`. ARM IRQ handler @kernel `0xaf73c` (FCSE PID=0=identity) → dispatch
  `0x3018` (INTEGRITY IC object tree). CPLD/expbus = bridged secondary ICs. Timer: 64b @`0xC0000098`.

- **Physical memory map complete** (`docs/13`): from kernel MMU region table, identity-mapped.
  High regions: RAM `0x20000000`, CHIF-shmem `0x40000000`, IC1 `0x80F00000`, MAC-DMA `0xA1000000`,
  video? `0xB0000000`, ASIC core `0xC0000000`, IC0 `0xCEFF0000`, host/flash `0xF0000000`.
- **Per-device IRQ map RECOVERED** (`docs/12`): the type-2 **kernel BSP IODevice registry**
  (`IODI2C0-7`, `IODMAC0/1`, …) registers each device's IRQ as an immediate. I2C0-7=**8-15**,
  MAC0/1=**16/17**, timer=**7**, SPI=26-28, KCS=**44**, UHCI=51/52, IRC/KVM=**57**, PECI=**60**,
  MCTP=62, LPCCOM=40-42, pwrmgr=31. Lines <32→IC0, >=32→IC1. (Earlier 'not static' was WRONG -
  looked in app, not kernel BSP.) Also revealed new devices: MCTP, PECI, SES, LPC-COM, host-I2C.
- **`0xD1000000`** = timer/Wallace companion block (byte regs +0x00-0x38; DRAM/video geom config).

- **KVM/video regs enumerated** (`docs/14` RE pass 2): 2 ASIC blocks — **DVC capture `0x01F100C0`**
  (+status `0x01F10088`/`0x10070`/`0x10098`; sw status struct `0x39f68+0x4c` poll ≤0xF) + **host-video
  source/snoop `0x01F06000`** (~0x34B; control/status +0x00, video-timing +0x02-0x24 16b, mode-status
  +0x20 read, **indexed data port +0x28/+0x2A** 16/32b heavy RMW = VGA CRTC-style snoop, data/counter
  +0x2C/+0x30 32b — docs/14 RE pass 3). Framebuffer = `0xB0000000`. DT video node has both reg blocks.
  Per-index field semantics need HW correlation.
- **KVM/video mapped** (`docs/14`): `0xB0000000`-`0xB3FFFFFF` (64 MB) = **video-capture buffer
  region** (DVC framebuffers, 24 descriptors). DVC pipeline: capture(dvccap)->encode(dvcbit)->
  encrypt(AES/RC4)->stream(dvcnet) to IRC. Capture controller reg `0x01F100C0`. OpenBMC: reserved
  memory + custom KVM driver (like aspeed-video).

- **`0xF0000000`-`0xFFEFFFFF` region = lightly-used upper-address aperture** (flags=1, NOT flash/
  heap/peripheral). ~10 genuine word reads; top-of-space reg block `0xFF000000`. Earlier 'heavily
  used' was FALSE POSITIVE = crypto error codes `0xf16100xx` (kliteMem/rsa). Likely host-mem/PCI
  aperture. Corrected docs/13.
- **SPI boot flash controller FULLY mapped** (`docs/15`,**`17`**): register-file SMIF `gxekflsh`
  @`0xC0000200` (NOT a 0xF0000000 window). Abs regs CONFIRMED: CTRL`+0x00`(clr 0x3800), MODE`+0x02`
  (3=data/0=none), GO/BUSY`+0x04`(wr=launch, rd bit1=busy), OPCODE`+0x05`(unlock `0x55`/`0xAA` 1st),
  COUNT`+0x06`(≤0x40), ADDR`+0x08`(24b), FIFO`+0x40`(64B). Opcodes WREN`0x06`/RDSR`0x05`(WIP bit0)/
  PP`0x02`/erase`0xD8`. 2 waits: GO bit1, then RDSR WIP. Magic `0x10ADBEEF`; `.lastsectorremap`,
  `.mr___flashbuf`. `0x01F10000` 'SMIF' = separate HOST-BIOS access (romchf).

- **RTC = SOFTWARE CLOCK, no hardware RTC** (`docs/18`, corrects docs/05): Gromit-XE has NO
  calendar-RTC register. `0xC0000097`=timer latch/ack byte, `0xC00000A0`=chip-ID/rev reg (guarded
  vs ~`0x50000`), `0xC0000098`=64-bit timebase counter. Wall time = INTEGRITY *RealTimeClock*
  software object (`TheTimeHasNeverBeenSet`) seeded from the counter, set via **SNTP/NTP**. "Date
  off 70 years" bug = CLOCK_REALTIME=1970 until synced. OpenBMC: timer clocksource + timesyncd +
  fake-hwclock, **no RTC driver**. DT rtc node REMOVED.
- **Drivers written** (`rebuild/meta-ilo3/.../files/`): **irqchip `irq-gxe.c`** (root, 2 banks/64
  lines, IRQCHIP_DECLARE+set_handle_irq; ENABLE`+0x10`/DISABLE`+0x14` confirmed, PENDING`+0x0C`/
  EOI`+0x30` best-reading); **timer `timer-gxe.c`** (clocksource+sched_clock from 64-bit counter
  @`0xC0000098` confirmed; **clockevent** on Wallace `0xD1000000` IRQ36 - enable`+0x09`=0x80/status
  `+0x00`bit7 confirmed, reload`+0x04`/ack best-guess flagged CONFIRM-ON-HW);
  **UART `gxe-uart.c`** (earlycon + polled tty
  serial + console, `ttyGXE0`; regs data`+0x00`/status`+0x05` TXRDY bit5/RXRDY bit0);
  **I2C `i2c-gxe.c`** (8 engines, DMA/mailbox model from `0xe21c8` — docs/16; 1 adapter/engine,
  regs`0x1F10100+e*0x10`/buf`0x1F10180+e*0x10`[+0x200 e6,7]/desc`0x1F101E0+e*4` confirmed,
  16B cap, reset+status`0x2`=done confirmed; cmd-byte encoding + trigger best-guess
  CONFIRM-ON-HW, poll-based, IRQ8-15 avail); **SPI-NOR `spi-gxe-smif.c`** (spi-mem controller for
  boot flash `gxekflsh` @`0xC0000200` — docs/17; register-file opcode/addr/count/FIFO, all regs+
  opcodes CONFIRMED, spi-nor/mtd core drives exec_op, 64B FIFO windows; GO launch-value encoding +
  bulk-read opcode best-guess CONFIRM-ON-HW); **CPLD-gpio `gpio-gxe-cpld.c`** (system CPLD/pwrmicro
  `0x01F14000` as bit-addressable gpio_chip — docs/08,09; gpio N=byte`N>>3`.bit`N&7`, 4096 lines,
  RMW get/set, sw direction; UID LED=gpio6 `+0x00`.6 CONFIRMED→gpio-leds `<&cpld 6 0>`; PSU
  enable/present gpio192-195/200-203 inferred; RMW-vs-iod_cpld + button-IRQ CONFIRM-ON-HW/future);
  **PECI `peci-gxe.c`** (native PECI engine @`0x01F00400` — docs/26, CORRECTS earlier 0x01F14068
  which is only the XNC MUX; CMD`+0x00`/STATUS`+0x04`/timing`+0x08`/enable`+0x0C`=0x80, IRQ60, rev
  strap `0x01F100A0`→timing 0x3d090010/0x61a80019, 0xD1=timeout all CONFIRMED; Linux peci_controller,
  descriptor-DMA xfer, desc layout best-guess CONFIRM-ON-HW; feeds dbus-sensors peci-cputemp);
  **fan-hwmon `fan-gxe.c`** (CPLD PWM/tach SERIAL scan-chain, NOT flat regs — docs/27; fan block
  `0x01F00200`: CTRL`+0x0C`/DATA`+0x10-0x17`/TACH`+0x18`=0xFA/`+0x1C`, latch=CPLD `0x01F1400B` bit1
  via hpe,cpld; RESYNC+regs CONFIRMED; pwmN/fanN_input→phosphor-pid-control; PWM duty-vs-FET
  semantics + per-fan chain bit + tach raw→RPM = CONFIRM-ON-HW);
  **GXELAN-MAC `gxe-gmac.c`** (2 NICs — docs/11; MAC0`0xC0004000` dedicated/MDIO, MAC1`0xC0005000`
  sideband/NC-SI. CONFIRMED+impl: MDIO `0xC0004080`(reg/phy/bit8=rd/bit9=go)/`0x84`, reset core
  `0xC00000C0`(MAC0 0x04/MAC1 0x80), addr`+0x18/1C/20`, mcast-hash`+0x24/28` CRC32 0x04C11DB6,
  ctrl`+0x14`. TX/RX DMA ring/descriptor format NOT statically recoverable (INTEGRITY DMA objects)
  → xmit/NAPI scaffolded, needs HW bring-up). Wired via
  bbappend (Kconfig/Makefile splice + cfg). dtc-clean. NOT compile-tested (no kernel tree here).
- **Devicetree draft consolidated** → `rebuild/meta-ilo3/` (OpenBMC layer): `ilo3-gxe.dts`
  (compiles clean w/ dtc, 41 nodes) from docs/02-15 — CPU arm926ej-s, 2-bank irqchip (64 IRQs),
  UART/RTC/crypto/timer/SMIF/MAC0-1/I2Cx8/CPLD/KCS/video/CHIF + per-device IRQs + reserved mem.
  Plus layer.conf, machine conf (armv5te), kernel bbappend, README. All `hpe,gxe-*` = net-new drivers.

## Firmware image format (`ilo3_194.bin`, in progress)

- Magic: ASCII `iLO3 v 1.94.2 06-Dec-2020\x1a` at offset 0; `file` recognizes HPE iLO3 image.
- Header 0x00–0x40 holds version string + a section-offset table (0x7e0000 cert tail, 0x800000 sig block).
- Payload from 0x40 is **HP LZSS-compressed** (not encrypted; `tools/ilo_lzss.py` decompresses it). Zero-pad gap @0x670000–0x6e0000.
- Cert/signature tail @0x7e0000–EOF (DER + PEM certs). Image is signed → matters for re-flash.
- Compare against public **Synacktiv ilo4_toolbox** (iLO4 .bin parser) — shared HP ROM lineage.

## Repository Layout

```
firmware/          # original downloaded images (.rpm, .scexe) — DO NOT edit; source of truth
  staging/         # working copy of extracted flash image (ilo3_194.bin) + xml
extracted/rpm/     # rpm2cpio|cpio output tree
decompiled/        # decompiler output (Ghidra/IDA exports)  [create as needed]
docs/              # findings, memory map, binary format notes
tools/             # unpack/patch/repack scripts  [create as needed]
rebuild/           # modified sources + build/flash artifacts
  meta-ilo3/       # OpenBMC layer draft: ilo3-gxe.dts devicetree + machine conf (from docs/02-15)
research/          # notes, datasheets, upstream references
openbmc/           # upstream OpenBMC clone (read-only reference) — NOT our code
  meta-hpe/        # HPE layer: meta-gxp BSP + Gen11 machine layers (port template)
  meta-phosphor/   # core OpenBMC distro policy + phosphor service recipes
  meta-aspeed/     # ASPEED BSP — useful as a second, more complete SoC-port example
```

`openbmc/` is a shallow (`--depth 1`) clone of `github.com/openbmc/openbmc`, no submodules.
Treat it as **upstream reference, read-only** — do not commit changes into it. Port work for
iLO 3 belongs in a separate layer under `rebuild/` (e.g. `rebuild/meta-ilo3/`) that layers on
top, not in edits to the clone. Refresh with `git -C openbmc pull --depth 1`; re-clone deeper
(`--depth 50` or full) if history/blame is needed.

## Tooling (expected)

- **Ghidra** / **IDA Pro** / **radare2 / rizin** — disassembly and decompilation.
- **binwalk** — identify and extract embedded filesystems/compressed blobs from images.
- **arm-none-eabi** or similar cross toolchain — if rebuilding.
- **BitBake / Yocto** (`openbmc/setup`, `oe-init-build-env`) — building OpenBMC images.
  Needs a Yocto-capable host (build deps + ~50 GB disk per machine target).
- **devicetree compiler (`dtc`)** — reading/writing the board description for a port.
- **`tools/ilo_lzss.py`** — HP iLO LZSS decompressor + ImgHeader parser (`--verify` self-checks).
- Keep a record in `docs/` of which tool and version produced each artifact.

## Working Conventions

- **Never modify original firmware images.** Copy before operating; treat `firmware/` as read-only source of truth.
- Note the target architecture, endianness, and load address before interpreting any disassembly.
- Record every assumption about the binary (base address, entry point, section boundaries) in `docs/` — decompiler guesses are often wrong until verified.
- Prefer small, documented, reversible transforms over large rewrites.

## Legal Note

This is reverse engineering of software for **interoperability, repair, and continued
operation of hardware the owner possesses**. Keep work focused on that purpose. Do not
redistribute HP's original copyrighted binaries.
