# iLO 3 CHIF host-interface server — BMC-side design (for OpenBMC)

Date: 2026-10-20. Synthesis of `docs/06` (PCI functions) + `docs/07` (protocol) + this
pass (both doorbells resolved). CHIF = the Gromit-XE host↔BMC Channel InterFace. The
**host** side exists upstream (`drivers/misc/hpilo.c`); the **BMC** side is net-new and is
the gating work for vKVM/vMedia. This doc specs what a port must build.

## 1. What it is / why net-new

Gromit-XE is a multifunction PCI endpoint on the host (docs/06): `.0` ISS/VAS, `.2` CHIF
messaging, `.4` virtual USB (UHCI). All three ride ONE shared-memory + doorbell transport
("CHIF"). The host OS talks to the BMC through it (IPMI-over-PCI, health, virtual media,
vKVM input). OpenBMC has **no** BMC-side CHIF server — it must be written from scratch.

## 2. Hardware / memory interface (all CONFIRMED, physical = identity-mapped, docs/10)

| Reg | W | Dir | Role |
|-----|---|-----|------|
| `0x01F02050` | 32 | init | **PCI BAR** register: host window = `(phys − 0x40000000) \| log2(size)` |
| `0x01F020D0` | 32 | init | CHIF control 0 (stored into each channel descriptor `+0x08`) |
| `0x01F020D4` | 32 | init+**TX** | init writes `0x04000000` (enable); **runtime writes `(1 << channel)` = BMC→host doorbell** (notify host of a msg on channel N) |
| `0x01F03004` | 16 | **RX** | doorbell status; **bit2 (0x4) = host→BMC message pending** |

- **Shared memory**: a physical region (~`0x40000000`, cached in RAM global `0x62a54`) mapped
  to the host as PCI `.2` BAR2 (**1 MB @ f1e00000** on the host). The BMC obtains it from the
  INTEGRITY kernel and programs `0x01F02050`. Both sides read/write CCBs + message buffers here.

## 3. Protocol structures (offsets CONFIRMED, docs/07)

- **Channel descriptors** — 10 × `0x4C` bytes, BMC RAM `0x62A7C`:
  `+0x04` state, `+0x08..+0x27` config block (copied from the CCB at open), `+0x28` u16
  **channel id** (`0xFFFF` = free), `+0x2A..+0x4B` ring state.
- **CCBs** (Command Control Blocks) — 32 × `0x80` bytes, in shared memory (base `*0x62A54`):
  `+0x04` u32 **message offset** into the window, `+0x10`/`+0x28` u32 ring control (**bit31 =
  ownership** / producer-consumer handoff).
- **Message**: `msg = shmem_base + CCB.offset`; a `0x4000` (16 KB) data region, then a header at
  `msg+0x4000` whose `+0x06` u16 = **channel id** (matched to descriptor `+0x28`). Size bounded
  to `0x4000`.

## 3a. Ring model + message framing (RE pass: `0x151484`/`0x1515f8`)

**No explicit head/tail ring indices.** CHIF flow control is the **CCB ownership bit** (bit31 of
CCB `+0x10`/`+0x28`): producer sets it, consumer clears it — a single-entry handshake per CCB
across the 32-CCB array. The descriptor `+0x2A..+0x4B` is **not** a ring: `+0x2A` (u16) = the
channel's **buffer size** (max payload = `+0x2A − 0xC`), the rest is config/queue state.

Message framing:
- **Global control header** at `shmem_base + 0x4000`: 4-byte **magic** — `"CHIF"` (`0x46494843`,
  msg type `0x18`) or `"CHI5"` (`0x35494843`, msg type `0x19` = ChifLite/v5) — followed by a
  **14-byte checksum** (sum of the 14 header bytes `& 0xff == 0`).
- **Per-message header** (at the CCB's message region): **length @ +0x00** (u16), **channel-id
  @ +0x06** (u16, matched to descriptor `+0x28`).
- RX copies the validated message out of shared memory into the target service's **software queue
  (depth 4)** ("queue full for chif service"); the service is then signalled.

So a BMC server needs: CCB ownership handshake (not ring pointers), the `CHIF`/`CHI5` magic +
14-byte checksum, the length/channel-id header, and a small per-channel receive queue.

## 4. Flows

### RX (host → BMC)
1. Doorbell IRQ or poll `0x01F03004`; **bit2 set ⇒ message pending** (else nothing).
2. Fetch active CCB → `+0x04` offset ⇒ `msg = shmem_base + offset`.
3. Read channel id `msg+0x4000+0x06`; linear-scan 10 descriptors for `+0x28 == id`.
4. Dispatch to that channel's handler (`chif_handler` task, entry `0x12a74`); flip CCB
   ownership bit (`+0x10`/`+0x28` bit31) back to the host when consumed.

### TX (BMC → host)
1. Write the response into the CCB's shared-memory message region; set the channel id in the
   header; claim ownership (CCB bit31).
2. **Ring the host**: write `(1 << channel)` to `0x01F020D4` (`0x1532b8`). Message-type dispatch
   `FUN_0x152fe0` handles types `0x18`/`0x19`/`0x1A`; the send helper (`chif_post`, `chif_post
   error`) does the buffer copy + doorbell.

## 5. Channel ids + service mapping

**Channel ids are host-assigned, not BMC-hardcoded.** `chif_message_dispatch` (`0x150cac`) reads
the message header `(subtype @ +0x04, channel-id @ +0x06)`, linear-scans the 10 descriptors for
`+0x28 == channel-id`, and calls that descriptor's handler object (`[desc+4]`). The id + config
block (`+0x08..+0x27`) come from the host's CCB at channel open — so the port must follow the
**host hpilo convention** for per-service id numbering, not a fixed BMC table.

BMC-reserved ids (the only hardcoded ones):

| Channel id | Meaning |
|-----------|---------|
| **`0xFFFE`** | built-in **echo / loopback** test service ("Found chif echo service call") |
| **`0xFFFF`** | **free** descriptor marker (unused slot) |

Low ids `1`, `2`, `0x10` get extra per-subtype validation/logging in the dispatch (subtypes e.g.
`9`, `0xFE`) but are otherwise ordinary service channels. Up to **10 concurrent** channels.

### Service mapping (from docs/06)

| Service | PCI fn | BMC module | OpenBMC target |
|---------|--------|-----------|----------------|
| IPMI / KCS | (host KCS `0x01F0D400`) | — | `phosphor-ipmi-kcs` (already exists) |
| Host power / health / SMBIOS (ISS/VAS/RBSU) | `.0` 3306 | `rbsu.c`, `romchf` | smbios-mdr + power/sensors |
| CHIF messaging | `.2` 3307 | `chif.c`, `chif_handler` | **the new CHIF server** |
| Virtual USB (vMedia/kbd/mouse) — **UHCI** | `.4` 3300 | `uhci.c` + `eh_intf`→`chif_post` | **net-new vUSB/vMedia bridge** |

Keyboard+mouse channels are always-connected; media attaches dynamically (`eh_intf_submit →
chif_post`).

## 6. OpenBMC implementation plan

**Kernel side** — a GXE CHIF driver (`drivers/misc/`, mirror of `hpilo.c` for the BMC):
- Map the shared-memory region + program the BAR (`0x01F02050`); map the doorbell block.
- IRQ on the CHIF line (host-interface IRQ, see docs/12) or poll `0x01F03004` bit2.
- Expose channels to userspace (char devices / a netlink or D-Bus shim), move CCB-referenced
  messages both ways, drive the `0x01F020D4` doorbell on TX.

**Userspace** — map each channel to an OpenBMC service:
- IPMI messages → `phosphor-ipmi-host` (reuse KCS path where possible).
- Host power / inventory → `x86-power-control`, `smbios-mdr`, `dbus-sensors`.
- **vMedia / vKVM** → the hard part: a vUSB bridge presenting the host a UHCI device (the host
  already binds `uhci_hcd` to `.4`), fed by `obmc-ikvm` (video, docs/14) + a virtual-media
  backend. This is the single largest net-new effort of the whole port.

**Phasing**: (1) CHIF transport driver + loopback; (2) IPMI/health/power channels (reuse
existing phosphor services) — gets remote management working; (3) vMedia; (4) vKVM input +
`obmc-ikvm`. Use `hpilo.c` as the protocol oracle throughout.

## 7. Still open

- ~~Per-channel ring head/tail/sequence~~ **RESOLVED**: no head/tail indices — flow control is the CCB ownership bit (bit31); `+0x2A` = buffer size, rest = config/queue state (§3a).
- ~~Exact channel-id numbering per service~~ **RESOLVED**: host-assigned (descriptor `+0x28`), dynamic; BMC reserves `0xFFFE` (echo) / `0xFFFF` (free). Per-service numbering = host hpilo convention.
- Message-type semantics beyond `0x18`/`0x19`/`0x1A`.
- Whether the BMC can use the host-interface IRQ or must poll (docs/12 host-interface line).

## 8. Host-side tool (`rebuild/ilo3-chif/`)

Compiled host CHIF client for the XCP-ng 8.3 (x86_64) host — debug console +
host-side SPI reflash over PCI, the bring-up counterpart of
`firmware/src/dbgchif.rs`. Static-musl binary (dom0 has no guaranteed python3).
Supersedes the python reference `ilo3-rs/tools/ilo3-chif-console.py` (kept in sync).

- Maps the `.2` BAR2 (1 MB CHIF shared-mem window, §2); auto-finds `103c:3307`.
- Debug channel `0xDB90`; message framing = §3 (`+0x00` len, `+0x06` chan, `+0x08`
  payload) + the 14-byte sum-to-zero header @ `shmem+0x4000`.
- `flash`/`verify`/`recover` = host reflash + SPL RAM-boot; SPL region `>=0x7f0000`
  is firmware-write-protected, a full image is auto-clipped to the payload.

**Ownership handshake polarity (corrected).** Request + response share one CCB; the
bit31 ring-control baton: host sets it (BMC owns), the BMC writes the response and
**clears** it (host owns again = response ready) — net behaviour of `dbgchif_task`
(`send()` sets, `rx_release()` clears). The host therefore waits for the bit to go
**clear**, not re-set. (The original python polarity waited for a re-set — fixed in
both tools.)

**Doorbell (still CONFIRM-ON-HW).** The host→BMC doorbell (`0x01F03004` bit2 on the
BMC) is raised from a register in a small `.2` BAR; exact BAR/offset/value not yet
pinned (oracle: `hpilo.c` `hpilo_db_outbound`). Tool takes `--db-bar/--db-off/
--db-val`; `observe` and an ownership-polling firmware work without it.

## 9. HARDWARE-CONFIRMED protocol (live DL380p G7, 2026-10-25) — hpilo oracle + lspci

First power-on of a host-reflashed board. `lspci` + `drivers/misc/hpilo.c` (v4.19)
+ live BAR reads give the REAL host-side protocol. **This overturns parts of §2-§3.**

**BAR map of `.2` (103c:3307, subsystem 103c:3309), confirmed on hardware:**
| BAR | addr/size (this board) | role (hpilo `ilo_map_device`) |
|-----|------------------------|-------------------------------|
| 0 (I/O) | `0x3800`/256 | I/O ports |
| 1 (mem) | `f1fe0000`/**256** | **MMIO regs** = BMC host-iface block (`0x01F02000`). `DB_IRQ`@`+0xB2`, `DB_OUT`@`+0xD4`. Live: `+0x50`=1, `+0x54`=0xf800000c, `+0xD0`=0x0200ffff. |
| 2 (mem) | `f1fd0000`/**4K** | **shared memory / CCBs** (bar=2 unless subsystem==0x00E4→bar=5) |
| 3 (mem) | `f1f00000`/**512K** | **per-CCB doorbell aperture**, stride `ONE_DB_SIZE`=`1<<14`=0x4000 |
| 4,5 | 32K each | — |

**Doorbell (host→BMC), CONFIRMED — resolves the §8 CONFIRM-ON-HW item:**
- Per channel: `iowrite8(1, BAR3 + channel*0x4000)` to notify; `iowrite8(2, …)` clears.
- Global `DB_OUT` = `BAR1 + 0xD4` (write bit `DB_RESET`=26 region); `DB_IRQ`=`BAR1+0xB2`.
- i.e. the host rings via the **BAR3 aperture byte-write**, NOT a single reg; the BMC side
  surfaces it (our RE's `0x01F03004` bit2 is the BMC view).

**CCB = 128 B hardware (`ILOHW_CCB_SZ`), 64 B software. Field offsets (CONFIRMED):**
`send_fifobar`@0x00, `send_desc`@0x08, `send_ctrl`@0x10, `recv_fifobar`@0x18,
`recv_desc`@0x20, `recv_ctrl`@0x28, `db_base`@0x30, `channel`@0x38.

**Transport = FIFO + descriptor rings, NOT a single ownership-bit CCB:**
`NR_QENTRY`=4 per fifo; entry bitfields `QWORDS`@0, `DESCRIPTOR`@10, `C`@22, **`O`(owner)@23**.
`SENDQ`=1/`RECVQ`=2. `fifo` handle + `desc_mem` (8-byte entries, `L2_QENTRY_SZ`=12).
`Open could not dequeue a packet` = hpilo put a pkt on SENDQ, rang the doorbell, BMC never
dequeued (MAX_WAIT_TIME=10000us).

### Impact on our firmware (ACTION)
Our `chif.rs`/`dbgchif.rs`/`recovery.rs` model (1 MB window @0x40000000; 0x80-stride CCB;
ownership **bit31**; regions at 0x4000/0x8000/0xC000) **does not match hardware**: the real
shared BAR is **4 KB** (BAR2), the doorbell is the **BAR3 aperture**, ownership is **bit23** of
descriptor entries, and framing is **send/recv FIFOs + descriptor rings**. Offsets like
`req@0xC000` don't exist in a 4 KB BAR. Consequences:
- Host↔BMC CHIF (incl. our tool's debug channel + host reflash + SPL recovery mailbox) cannot
  work as flashed — all assume the wrong window/CCB/doorbell.
- **Rework** chif/dbgchif/recovery to the hpilo FIFO/descriptor/doorbell model above, then
  reflash. The recovery mailbox fits in the 4 KB BAR2 (escape hatch to avoid re-soldering —
  IF the already-flashed SPL exposes it correctly; otherwise SPI programmer).
- Liveness of `main`/DDR is still unconfirmed remotely — use the UID-LED boot trace (docs/08)
  and iLO NIC/DHCP presence (model-independent), since no CHIF path is trustworthy yet.

## 10. HP's ACTUAL window-open, decompiled (ilo3_194.bin @ 0x14fc40) — ROOT CAUSE

Decompiled HP's CHIF init (the fn whose literal pool holds BAR_REG/CTRL0/CTRL1/
shmem-base/chan-desc, pool @0x14fed8-0x14ff04). This is clean-room ground truth
for the window-open our firmware got wrong.

**Sequence (0x14fc40):**
1. `bl 0x18e1edc` — INTEGRITY maps a shared-mem region → access base to `[sp,#0xc]`.
2. `bl 0x18c2570` — region phys **start → `[sp,#0x18]`, end → `[sp,#0x1c]`**.
3. `*(0x62A54) = base` — BMC-side access base cached (docs/07).
4. **BAR reg (0x01F02050) computation:**
   - `size = (end+1) - start`
   - `size >>= 12` (÷4096), then `r1 = bit_length(size/4096)` (count >>1 to zero)
   - `base_off = start - 0x40000000`
   - `*0x01F02050 = base_off | r1`
5. First `0x1000` of the window = the **32 CCBs** (0x80 each); **data/message region
   = base + 0x1000**. Control struct @`0x62A58` = {base, base+0x1000, 0x01F020D0,
   0x01F020D4, 0}.
6. 10 channel descriptors @`0x62A7C`, stride **0x4C**, `+0x28` = chan id (init `0xFFFF`).
7. CTRL1 `0x01F020D4 = 0x04000000` (enable); status/ack halfword @`0x01F020B2`
   (`ldrh [CTRL1-0x22]; tst #1 → write 1`).

**BAR SIZE ENCODING (the bug):** field = `bit_length(size/4096)`, so
`size = 4096 << (field-1)`. HP's 1 MB window → **field = 9**. The host PCI
enumerates the BAR at this size (set during iLO boot, before host power/POST).

**OUR FIRMWARE WAS WRONG** (`firmware/src/chif.rs` init):
`wr32(BAR_REG, (phys-0x40000000) | log2_size)` with `log2_size = 20` → field=20 →
`size = 4096<<19 = 2 GB` → invalid → **ASIC defaults BAR2 to 4 KB**. This is why the
live board (docs/28 §9) enumerated BAR2 = 4 KB and every window offset (header
0x4000 / recovery 0x8000 / req 0xC000) overflowed the host aperture → total host-
interface failure. **One constant.**

### FIRMWARE FIX (chif.rs + spl)
- Reserve a **size-aligned** shared region ≥ `0x40000000` in DRAM (1 MB, 1 MB-aligned).
- `*0x01F02050 = (phys - 0x40000000) | bit_length(size/4096)` (1 MB → `|9`).
- **Program the BAR size BEFORE host PCI enumeration** — in the SPL / earliest main,
  before host power is released (iLO gates host power, so this is reachable).
- Keep layout: CCBs in first 0x1000 (32×0x80), data region at base+0x1000; 10 chan
  descriptors in BMC RAM (0x4C stride, +0x28 chanid); CTRL0/CTRL1 + 0x020B2 ack.
- dbgchif/recovery offsets (0x4000/0x8000/0xC000) then fit inside the real window.

### Residual risk + de-risk (post-fix, 2026-10-26)
The BAR size-field fix (field = `bit_length(size/4096)`) is applied to `chif.rs` +
`spl/src/recovery.rs`; the SPL programs it before host PCI enumeration; the recovery
mailbox moved to the window START (first 4 KB) with a long (~minutes) wait for the
AC-cycle→host-boot timing. The host tool mailbox offset/chunk match.

**Residual unknown:** whether `0x40000000` (the CHIF-shmem window base) is backed by
our firmware's DRAM. HP's INTEGRITY allocated backed RAM there; our DRAM is decoded
at `0x20000000` and static RE did not confirm a `0x40000000` alias/aperture. The
size-field bug alone explains the observed dead BAR (invalid field ⇒ window disabled
⇒ stub 4 KB, zero + unwritable), so the fix is likely sufficient — but before trusting
a resolder, add an **SPL self-test**: write/readback a pattern in the `0x40000000`
window and blink a distinct UID-LED PASS/FAIL code (model-independent, no host channel).

### Doorbell-free debug/reflash channel (2026-10-26)
`dbgchif` now drives RX via `Chif::poll_rx_polled()` — it scans CCB ownership
directly, WITHOUT the `0x01F03004` bit2 doorbell gate (`chif.rs`). So the host tool
can drive the debug channel (incl. host SPI reflash: ferase/fwrite/fread) with only
a correctly-enumerated BAR window — no BAR3 doorbell needed. `header_ok()`
(magic+checksum) + `rx_release()` (clears ownership per request) keep it safe from
re-triggering on a stale header. Mirrors the doorbell-free SPL recovery mailbox.
Binary-verified: the running firmware contains no `0x01F03004` reference. The
production `poll_rx` (doorbell-gated, for stock-hpilo interop) remains for the §9
rewrite but is currently dead-stripped (no live caller).
