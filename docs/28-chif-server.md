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
