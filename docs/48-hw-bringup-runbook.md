# iLO 3 (Gromit-XE) Rust firmware — hardware bring-up runbook (SPI flasher)

Date: 2026-10-21. Step-by-step to flash + bring up the clean-room Rust firmware (`rebuild/ilo3-rs`)
on a real **HP ProLiant DL380p Gen7** iLO 3, using an external SPI flash programmer. Written for a
board you **own** (interop/repair). Read the whole thing before touching hardware — the irreversible
risks (wrong voltage, wrong chip, no backup) are all in Phase 0.

## 0. The two flashing paths

- **Path A — full replace (what we build).** `tools/pack-flash.py` makes an 8 MB image with our SPL
  at the reset vector (`0x7f0000` = `0xFFFF0000`) + the main firmware payload at `0x0`. The ARM926
  boots straight into our SPL — HP's signed boot ROM is gone. **No signature patch needed** (our SPL
  *is* the first stage; the sig-check only existed in HP's ROM which we overwrote). This runbook = Path A.
- **Path B — patch HP's ROM (alternative).** Keep HP firmware, NOP the sig-check branch
  (`ilo3_194.bin` offset `0x7f38bc`: `1a000040`→`e1a00000`, docs/31) and append our image. More
  fragile; only if you want HP's stack coexisting. Not covered further here.

## Confirmed hardware (this board)

| Part | Device | Notes |
|------|--------|-------|
| Boot flash | **Macronix MX25L6455EMI** | 8 MB (64 Mbit) SPI NOR, **3 V** (2.7–3.6 V), SOP-16 package. JEDEC RDID `C2 20 17`. This is the chip you clip/socket. |
| SRAM | ISSI IS62WV2568BLL-45 | pre-DDR scratch @ `0x20000000` (why SP `0x20004000` works at reset) |
| DRAM | ISSI IS43/46LD16640A | 1 Gbit LPDDR2-S4, 128 MB, RL7/WL4 (~400–466 MHz) |
| SoC | HP Gromit-XE, ARM926EJ-S (ARMv5TE) | reset → `0xFFFF0000` (flash `0x7f0000`), high vectors |
| Console | custom 8-bit UART @ `0xC00000F0` | **physical pins/baud = UNKNOWN — find first (Phase 3)** |

## Equipment

- **SPI programmer** with selectable Vcc: a CH341A (set the **3.3 V** jumper — never 5 V on this 3 V
  part), or better a flashrom-supported programmer (FT2232H / Dediprog) for reliable 8 MB reads.
- **SOIC-16 (300-mil) test clip** (chip is SOP-16) OR a SOIC-16→DIP socket if you desolder.
- **Ideally a spare MX25L6455E** (or any 8 MB SPI NOR) to flash experiments on, swapping the
  original out untouched — zero risk to the board's original.
- **3.3 V USB-serial adapter** (for the console; Phase 3).
- `flashrom` (preferred) or the programmer's GUI; a logic analyzer/scope helps find the UART.

## Phase 0 — SAFETY (do every step; this is the irreversible part)

1. **Power down** the server fully; pull AC (iLO is standby-powered — unplug, don't just soft-off).
2. **Identify the right chip.** There are multiple SPI flashes on a G7 board (host BIOS + iLO).
   Find the **iLO** flash: the MX25L6455E near the Gromit-XE ASIC. Read the printed part number;
   confirm `RDID` once connected = `C2 20 17`. **Do not write a chip whose RDID you have not confirmed.**
3. **Match voltage.** MX25L6455E is a **3 V** part. Set the programmer to **3.3 V**. A 5 V programmer
   (default on many CH341A) **will damage the chip**. Verify with a meter before clipping if unsure.
4. **FULL BACKUP — twice.** With the clip on (board unpowered):
   ```
   flashrom -p <programmer> -c "MX25L6455E" -r ilo3_orig_1.bin
   flashrom -p <programmer> -c "MX25L6455E" -r ilo3_orig_2.bin
   sha256sum ilo3_orig_1.bin ilo3_orig_2.bin   # MUST match each other
   ```
   Both reads must be **8388608 bytes** and **identical**. Store multiple copies off-machine. This
   backup holds board-specific NVRAM/config/cert that the distributed `firmware/ilo3_194.bin` does
   NOT — it is the only route back. **No verified backup ⇒ stop.**
5. (Recommended) **Flash the spare chip, not the original.** Do all of Phase 2 on the spare, swap it
   in, and keep the original in a drawer. Recovery = swap the original back.

## Phase 1 — build the flash image

```
cd rebuild/ilo3-rs
cargo +nightly build --release                       # firmware + spl crates
OC=$(find $(rustc --print sysroot) -name llvm-objcopy | head -1)
$OC -O binary target/armv5te-ilo3/release/ilo3-fw  ilo3-fw.bin
$OC -O binary target/armv5te-ilo3/release/ilo3-spl ilo3-spl.bin
python3 tools/pack-flash.py ilo3-fw.bin ilo3-spl.bin flash.bin
```
Verify the image before flashing:
- `stat -c%s flash.bin` = **8388608**.
- bytes @ `0x0` = `47 58 53 50` ("GXSP") + length + load/entry `0x20000000`.
- bytes @ `0x7f0000` = `ea0000xx` (the SPL `b _reset` vector) — the reset target.
- tail (`0x7ffff0`) = `0xff` padding.

(`od -A x -t x1 -N 16 flash.bin` and `od -A x -t x1 -j 0x7f0000 -N 16 flash.bin`.)

## Phase 2 — flash (spare chip preferred)

```
flashrom -p <programmer> -c "MX25L6455E" -w flash.bin   # erases + writes + verifies
```
`flashrom` verifies after write; if it reports VERIFIED, the chip matches `flash.bin`. If you flashed
a spare, solder/socket it onto the board now. Do NOT power on until the console tap is ready (Phase 3)
— you want to see the first boot.

## Phase 3 — console + first boot

The firmware logs the whole bring-up over the Gromit-XE console UART (`0xC00000F0`). **Finding the
physical pins + baud is the first on-hardware task** — without it you are blind.

1. **Locate the UART.** With the **original** HP firmware still in (or a second board), power iLO and
   scope the candidate service-header / test-point pins for a 3.3 V TTL serial idle-high line that
   bursts at HP's boot banner. TX is the pin with activity shortly after standby power. Note the pin
   + GND.
2. **Measure the baud** from HP's output (the custom UART has no baud register, docs/05 — the rate is
   strap/clock-fixed). A scope on the shortest bit width gives it (common: 115200; confirm).
3. **Tap it**: 3.3 V USB-serial RX ↔ board TX, GND ↔ GND. Terminal at the measured baud, 8N1.
4. **Power on** with our image flashed. Expected sequence (our banners):
   ```
   [spl] iLO3 Gromit-XE first-stage loader
   [spl] clock mode 0x...  (strap-default clock, no PLL reprogram)
   [spl] ddr: cold init complete            <-- or "warm boot"
   [spl] load 0x...B -> 0x20000000
   [spl] boot -> 0x20000000
   [ilo3-rs] Gromit-XE bring-up — hello from Rust on ARM926EJ-S
   [ilo3-rs] mmu: on — identity 1 MB sections, DDR cached, high vectors @ 0xffff0000
   [ilo3-rs] heap: N KB DDR global allocator
   [ilo3-rs] rng: entropy pool seeded ... QUALITY UNVERIFIED
   [ilo3-rs] crypto: self-test PASS (ChaCha20-Poly1305 / SHA-256 / X25519)
   [web] default account 'Administrator' password: XXXXXXXXXXXX   <-- RECORD THIS
   [ilo3-rs] thermal: boot fail-safe — fans MAX
   [hb] heartbeat 0 — monotonic=... irq_ticks=...
   ```
   - **Record the printed Administrator password** — it is the only credential until you change it.
   - **No output at all** → wrong pins/baud, or the SPL hung before UART (see Troubleshooting).
   - If the crypto self-test prints **FAIL**, an ARMv5 codegen issue broke a primitive — stop, do not
     trust crypto.

## Phase 4 — CONFIRM-ON-HW checklist (bring-up order)

Every item flagged CONFIRM-ON-HW across the RE/code, in the order the boot hits them. Tick each.

### 4.1 SPL / DDR / clock (docs/30)
- [ ] `clock mode` print is **non-zero** (`*0xD1000000 & 3`). If `0`, the board wants the strap→PLL
      decode path we did NOT implement — DDR may mistrain; see docs/30.
- [ ] `ddr: cold init complete` (not `PHY calibration TIMEOUT`). Timeout ⇒ training failed.
- [ ] Power-stable delay ≥ 200 µs: once the counter rate is known (4.2), confirm `0x1F4` ticks ≥ 200 µs.
- [ ] ASIC rev > 4 (`*0xC00000A0` bits[23:16]) — the docs/30 table path assumes this.
- [ ] After a thermal soak, no refresh-related corruption (validates `tREFI` = `0x48`=`0x7D`).

### 4.2 MMU / IRQ / timer
- [ ] No data-abort hang after `mmu: on` (map attrs correct).
- [ ] `irq_ticks=` **increments** in the heartbeat ⇒ live IRQ delivery + Wallace reload/ack correct.
- [ ] **Measure the real counter rate**: time N heartbeats vs a stopwatch → derive Hz. Fix
      `timer::TIMER_HZ` (embassy tick rate, all delays, the DDR 200 µs check depend on it).

### 4.3 heap / rng / crypto
- [ ] `crypto: self-test PASS`.
- [ ] **Entropy quality**: capture a long `rng::fill_bytes` stream, run a **NIST SP 800-90B** min-entropy
      assessment. Do NOT generate/trust TLS or host keys until this passes.

### 4.4 CPLD / power (docs/08,09,24)
- [ ] UID LED: `uid on` / `uid off` (CLP) toggles the physical UID LED ⇒ CPLD `+0x00` bit6 confirmed.
- [ ] `power` state read matches actual host power (`0x01F02070` bit24/25).
- [ ] Carefully: power on/off/reset/NMI drive the host as expected (force-off hold, warm-reset 2-byte,
      `0xB2` cold-cycle) — docs/24.

### 4.5 sensors / fan / PSU / drives (docs/20,23,25,27)
- [ ] Replace the **placeholder** `sensors::TABLE` bus/addr with the board SDR; temps read sane.
- [ ] `fan::set_pwm` changes fan speed; `read_tach_raw` → RPM (confirm scan-chain latch bit `0x01F1400B.1`,
      tach result reg + raw→RPM scale).
- [ ] **Thermal fail-safe**: pull/short a sensor → fans go MAX + an IML `SENSOR_FAIL`/`CRITICAL_TEMP`
      event appears in `json/server_event_log`. This is the safety-critical path — verify it.
- [ ] PSU panda read (bus/addr), presence via CPLD `+0x19`; SEP drive probe `0x70-0x77` + the `D8` frame
      checksum formula.

### 4.6 flash (spinor, docs/17)
- [ ] `spinor::read_id` == `C2 20 17`.
- [ ] NVRAM/cert/log store: write → power-cycle → read-back survives (confirms `erase_4k/64k` + the
      `GO` launch-value encoding).

### 4.7 networking — the gating item (docs/11)
- [ ] MDIO: read PHY ID register ⇒ MDIO regs + MAC0 base confirmed.
- [ ] **Reconstruct the MAC TX/RX DMA ring** — not statically recoverable (docs/11). On the live board,
      observe the descriptor memory + MAC ring registers (scope/JTAG, or diff against HP firmware
      running traffic), then implement `net::MacRing` for `GxeDevice`. **Until this works, NO packets
      flow** — DHCP/HTTPS/SSH/SNMP/KVM are all dark.
- [ ] After the ring: DHCP leases; `https://<ip>` login with the printed Administrator password; SSH
      CLI; SNMP walk; KVM via noVNC at `wss://<ip>:6080/?token=<session-bearer>`.

### 4.8 host interface (CHIF/KCS, docs/28,36)
- [ ] CHIF shared-window base + CCB ownership convention (needs host PCI); `chif_server` routing.
- [ ] KCS register offsets at `0x01F0D400` vs host `ipmitool`; SMBIOS fetch over ROM-CHIF (docs/22).

## Recovery

- **Reflash the backup**: `flashrom -p <programmer> -c "MX25L6455E" -w ilo3_orig_1.bin` (verify).
- **Spare-chip route**: swap the original chip back in. The board returns to stock iLO 3.
- A bad iLO flash does **not** brick the host server — iLO is standby-powered + independent; the host
  still boots. You only lose remote management until reflashed.

## Alternative: JTAG-to-DDR (no flash write)

If the Gromit-XE JTAG is exposed, you can validate the main firmware **without flashing**: bring up
DDR (run the `spl/src/ddr.rs` sequence as a JTAG script, or let a prior stage train it), load
`ilo3-fw.bin` to `0x20000000`, set PC there, run. Zero brick risk — best for fast iteration before
committing to flash. The SPL path (flash) is only needed for an unattended power-on boot.

## Troubleshooting

| Symptom | Likely cause / next step |
|---------|--------------------------|
| No console output at all | Wrong UART pin/baud (re-scope against HP firmware), or SPL faulted before UART. Scope the flash CLK pin at power-on — activity = SPL is executing XIP. |
| Hangs, last line `clock mode ...` | DDR init didn't finish; expect `PHY calibration TIMEOUT`. Check clock mode ≠ 0, power delay, that the part is the assumed LPDDR2-S4 (docs/30). |
| `boot -> 0x20000000` then silence | Copy/jump OK but main `_start` faulted. Confirm DDR actually presents RAM at `0x20000000` post-init (SRAM↔DDR overlay, docs/13); check the stacks/vector-alias region. |
| Resets/aborts after `mmu: on` | MMU attributes wrong (device vs cached section) or a data abort into the `_trap` spin. Re-check `src/mmu.rs` section flags vs the real peripheral map. |
| Fans never spin up | scan-chain latch bit / PWM-vs-FET semantics wrong (docs/27) — but fail-safe should still force MAX; if not, the fan HAL base/latch is off. |
| Everything boots, no network | Expected until 4.7 (MAC ring). Not a fault. |

## One-line summary of the HW-confirm priority

**UART tap → DDR/clock boots → counter rate → MMU/IRQ live → thermal fail-safe → MAC DMA ring
(unblocks all networking) → entropy assessment → host CHIF/KCS.** The MAC ring is the single biggest
unblocker; the thermal fail-safe is the single most safety-critical to verify.

## Interactive debugging — the serial debug shell

The firmware ships a **serial debug shell** on the console UART (`src/debugsh.rs`, the `debug-shell`
cargo feature, **default-on** for bring-up). It depends on nothing HW-gated — just the polled UART —
so it is the interactive bring-up console over the same serial tap as Phase 3. Commands:

```
md <addr> [len]   dump memory/MMIO (hex+ascii)   mw <addr> <u32>   write word (poke)
mb <addr> <u8>    write byte                      id   flash JEDEC (RDID)
temp              sensor reads                     power  host power state
log               IML event log                    help
```

Why this, not VSP: **VSP (the iLO virtual serial port) cannot be the bring-up debug channel** — it
bridges the *host's* serial over CHIF + the network, both of which are HW-gated (the very things
being brought up), and it carries the host OS console, not iLO's own debug output. Use the physical
console UART now; expose the shell over SSH/VSP only *after* CHIF + networking are validated.

`md`/`mw` are raw peek/poke over all address space — this is how you probe registers by hand and
**reconstruct the MAC DMA ring** (4.7): dump the MAC block + the descriptor memory while the ring is
active, poke candidate registers, confirm against the host `hpilo` driver. **DANGER / production**:
raw memory access over serial — build `--no-default-features` to exclude the shell from any shipped/
networked image (verified to compile both ways).

## Serial-over-CHIF debug channel (no JTAG/UART — host-pull)

With no JTAG and no tappable UART, the firmware's debug channel rides **CHIF (pure PCI)**, read
from the host. Pieces:

- **`src/dbglog.rs`** — a 32 KB RAM **ring buffer** that captures all `main`-onward debug output
  (boot banners teed via `dbglog::con()`). It is **host-PULL**: because iLO boots before the host,
  the host connects later and *drains* the ring, so early-boot output is **not missed**. (Pre-DDR/SPL
  phases, before RAM, still use the flash-breadcrumb.)
- **`src/dbgchif.rs`** — a CHIF debug service (channel `0xDB90`) with a request/response protocol:
  `PING`, `DRAIN` (pull the log ring), `MD addr len` (dump memory/MMIO), `MW/MB addr val` (poke).
  Feature-gated (`debug-shell`); spawned for bring-up.
- **`tools/ilo3-chif-console.py`** — host side. `observe` mode hexdumps the CHIF BAR (passive —
  use FIRST to confirm iLO is doing CHIF + to reverse the CCB/doorbell convention vs `hpilo.c`);
  `console` mode is the interactive `ping / drain / md / mw / mb`.

Procedure:
1. Flash the firmware; boot iLO + the host OS.
2. Free the region: `echo 0000:02:00.2 > /sys/bus/pci/drivers/hpilo/unbind`.
3. `sudo ./tools/ilo3-chif-console.py observe --bar /sys/bus/pci/devices/0000:02:00.2/resource2`
   → confirm the `CHIF` magic + a produced CCB (iLO's CHIF is alive). This also validates `chif.rs`
   without any on-iLO console.
4. Pin the two CONFIRM-ON-HW params from `hpilo.c` + observe output: the **host→iLO doorbell**
   (`ring_doorbell` in the tool) and the **CCB message-offset convention** (`REQ_MSG_OFF`).
5. `console` mode → `drain` to read the full early-boot log, `md`/`mw` to probe hardware (e.g. dump
   the MAC ring registers + descriptor memory to reconstruct the ring, 4.7).

This is the primary bring-up debug path on a board with no JTAG/UART: **passive BAR observe to get
CHIF up, then host-pull drain + peek/poke for everything after.**

## Reflash from host (no resolder)

Once a working firmware (with the CHIF debug channel) is on the chip via the SPI programmer,
iterate **without desoldering**: the host streams a new image over CHIF and iLO writes its own SPI
via the `spinor` driver. Safe because **iLO runs from DDR** (the SPL copied main to `0x20000000`),
so the flash is not executing while it is erased/rewritten.

Firmware commands (src/dbgchif.rs): `FID` (JEDEC id), `FERASE addr len`, `FWRITE addr len +data`,
`FREAD addr len`, `REBOOT` (MMU/caches off → jump `0xFFFF0000` = flash reset vector).

Host (tools/ilo3-chif-console.py, `console` mode):
```
chif> id                     # confirm the flash controller answers (expect c2 20 17)
chif> flash flash.bin        # erase + stream + verify the payload region
chif> reboot                 # iLO re-runs the flash boot → new payload
```

**Brick-proofing — the SPL/boot region (`>=0x7f0000`) is write-protected in firmware.** `FERASE`/
`FWRITE` refuse any range touching it (status `ST_PROT`), and the host tool clips a full `flash.bin`
to `[0, 0x7f0000)`. So:
- A bad reflash of the **payload** → reboot → SPL (always good) still runs → re-flash from host again.
- The **SPL** can only be written by the SPI programmer. Keep it stable.

**Remaining gap (follow-on): the early-main brick.** If a flashed payload crashes *before* the CHIF
debug task spawns, the channel never comes up and the host can't re-flash → back to the programmer.
To close this, add a **recovery window in the SPL**: after DDR init, before jumping to main, poll
CHIF for a few seconds for a recovery/flash request (minimal bare-metal CHIF + `spinor` write). Then
even a dead main is recoverable from the host. Until then: keep the verified backup + the programmer
as last resort, and prefer reflashing only after a change has reached the debug task at least once.

Clean reset alternative to `reboot`: pull AC / cycle iLO standby power (full reset); the soft-reboot
just saves the manual step (a true SoC/WDT reset is CONFIRM-ON-HW).
