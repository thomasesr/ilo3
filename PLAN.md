# PLAN.md — iLO 3 → OpenBMC port: work remaining

Goal: run **OpenBMC** on HP ProLiant **Gen7** servers (iLO 3 / Gromit-XE "GXE" BMC,
ARM926EJ-S / ARMv5TEJ), replacing the EOL HP firmware. This file tracks what is **left to
do**. Background + findings live in `docs/02`–`31`; the layer draft is `rebuild/meta-ilo3/`.

Legend: ☐ todo · ◐ partial · ☑ done · **HW** = needs real hardware · **BUILD** = integration
(no new RE) · **NEW** = net-new code with no upstream equivalent.

---

## 0. Status so far (done — reference, not work)

- ☑ **Reverse engineering** complete to register level across boot, SoC, sensors, power,
  inventory, host interface, KVM. SoC = Gromit-XE, ARM926EJ-S, Green Hills INTEGRITY. (`docs/02`–`31`)
- ☑ **9 Linux drivers written** (RE-derived, dtc-clean, NOT compile-tested): irqchip, timer,
  UART, I2C, SPI-NOR, CPLD-gpio, PECI, fan-scanchain hwmon, GXELAN MAC+MDIO.
  (`rebuild/meta-ilo3/recipes-kernel/linux/files/`)
- ☑ **Devicetree** (`ilo3-gxe.dts`, 40+ nodes, dtc-clean) + machine conf (armv5te) + bbappend.
- ☑ **Boot chain mapped**: strap/clock (`docs/30`), DDR3 init table (`docs/30`), SPI flash
  (`docs/15`), secure-boot + **patch point `0x7f38bc`** (`docs/31`).
- ☑ **Reflash feasibility proven**: no silicon root of trust; flash boot ROM = top of chain;
  single-instruction patch unlocks unsigned boot (`docs/31`).

---

## 1. Milestone A — Unlock the hardware (reflash capability)

Without this nothing else can run on real iron.

- ☐ **HW** Acquire an **external SPI flash programmer** (e.g. CH341A/Dediprog) + SOIC clip for
  the iLO 3 boot flash chip. Identify + locate the chip on a G7 board.
- ☐ **HW** Dump the stock flash; keep a **known-good backup** (recovery).
- ☐ **HW** Flash the stock image with the **sig-check NOP patch** at `0x7f38bc`
  (`1a000040` → `e1a00000`, `docs/31`) and confirm the box still boots stock firmware. This
  **proves no on-die mask ROM re-verifies the boot ROM** — the single most important hardware
  test in the whole project. If it boots: reflash is unlocked. If not: investigate the silicon
  root of trust (JTAG / reset trace) before going further.
- ☐ **HW** Confirm the boot ROM's UART console (`ttyGXE`, `0xC00000F0`, docs/05) prints on a
  physical header — the debug lifeline for everything below.

Exit criteria: can flash arbitrary code to the BMC and get boot-ROM serial output.

---

## 2. Milestone B — First-stage / U-Boot bring-up

Replace (or chain-load from) the patched boot ROM with an open first-stage that inits DRAM and
loads a kernel. Reference: the HPE `gxp-uboot` fork structure (`meta-gxp`), but GXE is different
silicon — new SoC support.

- ☐ **NEW/BUILD** Create a **Gromit-XE U-Boot** (or a minimal SPL). Port, from `docs/30`:
  - ☐ **Clock/strap init**: read strap `0x801F00A4`, program PLL `0x80200000+0x88`.
  - ☐ **DDR3 init**: reproduce the `0x7f198c` sequence + the (reg,val) table (warm-check `+0x20`
    bit7 → 500-tick delay → table phase1 → cal-wait bit7 → table phase2 → MEMCFG `0xC0000064`).
  - ☐ **Serial**: the GXE UART (`0xC00000F0`, polled, docs/05) for console.
  - ☐ **SPI-NOR**: the SMIF controller (`0xC0000200`, docs/15) to read the kernel from flash.
  - ☐ **Timer**: the 64-bit counter (`0xC0000098`) for delays.
- ☐ **HW** Confirm DRAM size from MEMCFG/strap (DT says 32 MB placeholder — `docs/13`).
- ☐ **BUILD** Decide boot layout: patched HP boot ROM chain-loads U-Boot, OR U-Boot replaces the
  boot ROM at `0xFFFF0000`. Lay out flash partitions (`docs/01`/`03`: boot, U-Boot, kernel,
  rootfs, env).

Exit criteria: U-Boot prompt over serial, DRAM trained, can load a file from SPI flash.

---

## 3. Milestone C — Linux kernel + the 9 drivers

- ☐ **BUILD** Stand up a kernel tree (OpenBMC's, or mainline) for **ARMv5TE** + the `meta-ilo3`
  bbappend; **compile-test all 9 drivers** (none built yet — no kernel tree in the RE env).
  Fix API drift (e.g. `uart_port_lock*`, `devm_spi_alloc_host` ≥6.8, `from_timer`).
- ☐ **NEW** New Gromit-XE devicetree support (mainline has no GXE) — the `ilo3-gxe.dts` is the
  start; add CPU/clock bindings as needed.
- ☐ **HW** Boot the kernel to an early console via **earlycon** (`hpe,gxe-uart`), then full tty.
- ☐ **HW — CONFIRM-ON-HARDWARE checklist** (driver fields flagged uncertain during RE):
  - ☐ irqchip (`irq-gxe`): PENDING `+0x0C` / EOI `+0x30` exact offsets.
  - ☐ timer (`timer-gxe`): clockevent **reload reg + ack** on Wallace `0xD1000000`; real clock rate.
  - ☐ I2C (`i2c-gxe`): command-byte encoding + the engine **trigger** (behind the fw IODevice).
  - ☐ SPI-NOR (`spi-gxe-smif`): the **GO launch-value** per op + bulk-read opcode.
  - ☐ CPLD-gpio (`gpio-gxe-cpld`): RMW-vs-`iod_cpld` races; wire the UID **button IRQ**.
  - ☐ PECI (`peci-gxe`): the **command-descriptor layout** + response-buffer offset.
  - ☐ fan (`fan-gxe`): **PWM duty-byte vs software-FET** semantics; per-fan chain bit map; tach
    raw→RPM scale.
  - ☐ MAC (`gxe-gmac`): the **TX/RX DMA ring/descriptor format** — unrecovered (INTEGRITY DMA);
    the datapath is a scaffold. This gates networking; needs HW + more RE.
  - ☐ video: capture resolution/framebuffer-base fields; `0x01F06000` per-index semantics.

Exit criteria: kernel boots to a shell on real hardware; UART + timer + irqchip solid.

---

## 4. Milestone D — OpenBMC base image (boot to BMC userspace)

- ☐ **BUILD** Finish `rebuild/meta-ilo3/` as a real Yocto machine layer:
  - ☐ Machine conf wiring (model on `meta-dl360poc`, the best HPE template — CLAUDE.md).
  - ☐ `DEFAULTTUNE = armv5te`, kernel recipe `linux-ilo3`, U-Boot recipe, flash-image layout.
  - ☐ Rootfs/packagegroup selection (phosphor minimal first).
- ☐ **BUILD** Get `MACHINE=ilo3-gxe bitbake obmc-phosphor-image` to produce a flashable image
  (currently "will NOT boot yet", README).
- ☐ **HW** Boot systemd + D-Bus to a login over serial. Networking via **GXELAN MAC** (blocked
  on the MAC ring format, §3).

Exit criteria: OpenBMC userspace up, D-Bus alive, SSH over the dedicated NIC.

---

## 5. Milestone E — Core BMC functions (sensors, power, fans, LEDs)

Data paths all mapped (`docs/08`,`09`,`22`–`27`); this is wiring them to phosphor services.

- ☐ **BUILD** **Temperatures** → `dbus-sensors`: LM75 over `i2c-gxe` (hwmon `lm75`+`pca954x`),
  CPU via `peci-cputemp` on `peci-gxe`. Translate the iLO **SDR topology → entity-manager JSON /
  DT** (per-sensor bus+addr are runtime SDR data, not in the binary — `docs/25`). **HW** to read
  actual addresses from a live SDR dump.
- ☐ **NEW/BUILD** **Fans** → `phosphor-pid-control` on the `fan-gxe` scan-chain hwmon (confirm
  PWM semantics first, §3).
- ☐ **NEW** **Power control** → `x86-power-control`: expose the host-iface/CPLD bits as GPIO
  lines (vPWRBTN `0x01F02046`, PGOOD `0x01F02070` bit24, reset seq `0x01F0005C/5D`, NMI
  `0x01F0006E`, CSM `0x01F000E7`, button CPLD `0x01F14008` — `docs/24`). Needs a **gpio-mmio
  shim** + custom actions for the 2-byte reset and `0xB2` cold-cycle.
- ☐ **BUILD** **UID LED + chassis** → `gpio-leds` on `gpio-gxe-cpld` (UID = gpio 6, `docs/08/09`).
- ☐ **NEW/BUILD** **PSU** → `phosphor-power`/`psu-manager`: the "panda" supplies are **HP-
  proprietary SMBus** (`docs/20`), NOT PMBus — write a custom panda PSU daemon on `i2c-gxe`
  (regs 0x00 fwrev / 0x02 status / 0x0A Iin / 0x32 peak / 0x3A ctrl), presence/enable via
  `gpio-gxe-cpld`. Check whether a std PMBus page also exists (would simplify).
- ☐ **BUILD** **Event/SEL log** → `phosphor-logging`.

Exit criteria: Redfish/ipmitool shows live temps, fan RPM, PSU status; power on/off/reset works.

---

## 6. Milestone F — Host interface (IPMI, inventory, CHIF)

- ☐ **BUILD** **IPMI/KCS** → `phosphor-ipmi-kcs` on the KCS block (`0x01F0D400`, `docs/05`).
- ☐ **NEW** **CHIF server** (the gating host-interface work, spec `docs/28`): a BMC-side kernel
  CHIF driver (mirror of host `drivers/misc/hpilo.c`) — map the 1 MB shared-mem BAR
  (`0x01F02050`), service the doorbells (RX `0x01F03004` bit2 / TX `0x01F020D4` `1<<chan`), move
  CCB messages (ownership-bit handshake, `CHIF`/`CHI5` magic + 14-byte checksum). Then userspace
  routing of channels → services.
- ☐ **BUILD/NEW** **CPU + memory inventory** → `smbios-mdr` (MDRv2): ingest the host SMBIOS table
  the host streams over ROM-CHIF (`docs/22`) → Redfish `Processors`/`Memory`.
- ☐ **NEW** **Drive backplane** (`docs/23`): a userspace shim emitting the SEP's proprietary
  `D2/D8` I2C frames on `i2c-gxe` → `Inventory.Item.Drive` + `Led.Group`. (Not SES/SAF-TE.)

Exit criteria: host OS sees the BMC via IPMI; inventory + drive status in Redfish.

---

## 7. Milestone G — Remote console (KVM video + virtual media)

The largest net-new subsystem; depends on the CHIF server (F).

- ☐ **NEW** **KVM video capture** driver (like `aspeed-video`): the DVC capture engine
  (`0x01F100C0`) + host-video snoop (`0x01F06000`) + `0xB0000000` 64 MB framebuffers → a V4L2
  source for **obmc-ikvm** (`docs/14`). **HW** to pin resolution/framebuffer-base + the
  `0x01F06000` per-index fields.
- ☐ **NEW** **vUSB / UHCI emulation** (`docs/29`): emulate the `.4` UHCI controller toward the
  host + walk the host frame-list/TD schedule over CHIF; present **HID keyboard/mouse** +
  **USB mass storage** (BOT/SCSI). Possibly reuse Linux USB-gadget `f_hid`/`f_mass_storage`.
- ☐ **NEW/BUILD** **Virtual media backend**: bmcweb **virtual-media / nbd-proxy** → mount
  `.img`/`.iso` from a URL into the mass-storage device.
- ☐ **BUILD** **bmcweb / Redfish + modern TLS** — the whole point (fixes iLO 3's dead crypto):
  stock OpenBMC bmcweb, remote console via obmc-ikvm WebSocket.

Exit criteria: browser remote console (video + keyboard/mouse) + mount an ISO + boot from it.

---

## Cross-cutting / housekeeping

- ☐ **BUILD** CI: build the layer, run `dtc`, compile the drivers, basic boot test (qemu where
  possible — limited for custom SoC).
- ☐ **BUILD** Upstreaming: `PORT_GXE` serial id, driver submissions, a `meta-ilo3` layer PR.
- ☐ Keep `docs/` as the authority; update CLAUDE.md status as milestones land.
- ◐ Small **open RE items** (low priority, not blockers):
  - MAC TX/RX descriptor format (behind INTEGRITY DMA — likely needs HW/tracing).
  - RSA key size + cert chain depth (`docs/31`, not needed for the patch).
  - A few CONFIRM-ON-HW register fields (consolidated in §3).

## Risks / unknowns

- **R1 (critical):** an undisclosed on-die mask ROM re-verifies the boot ROM → kills the reflash
  path. Mitigation: Milestone A hardware test decides it early. All static evidence says no.
- **R2:** MAC DMA ring format not recoverable statically → networking needs HW bring-up + tracing.
- **R3:** The host-facing stack (CHIF + vUSB + KVM) is large net-new work with no OpenBMC
  equivalent — the long tail. Management (IPMI/power/sensors) works without it; prioritise that.
- **R4:** Some driver fields are best-guess (§3); each needs one HW confirm.
- **R5:** Toolchain/era: ARMv5TE + an old INTEGRITY-shaped board; expect OpenBMC assumptions
  (ASPEED-centric) to need patching.

## Suggested order

A (unlock) → B (U-Boot/DRAM) → C (kernel+drivers) → D (OpenBMC base + network) → E (sensors/
power/fans/LED/PSU) → F (IPMI/inventory/CHIF) → G (KVM/vMedia). A–E deliver a usable managed
server; F–G restore full remote console.
