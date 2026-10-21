# iLO 3 CHIF protocol — channel descriptors, CCB, doorbell

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`ARM:LE:32:v5`, base 0),
`src\chif.c`. CHIF = the Gromit-XE host↔BMC Channel InterFace behind PCI function `.2`
(`103c:3307`), shared-memory BAR `.2`/R2 (1 MB @ `f1e00000` on the host). Companion to the
host-side Linux driver `drivers/misc/hpilo.c` (which implements the mirror of all of this).

Confidence: register addresses and struct *offsets* are read directly from disassembly; field
*semantics* marked (inferred) are best-effort.

## Doorbell / control registers (BMC virtual addresses, `0x01F0xxxx` window)

| Reg | Width | Role | Evidence |
|-----|-------|------|----------|
| `0x01F02050` | 32 | **PCI BAR register** — host window base/size: `(phys − 0x40000000) \| log2(size)` | chif init stores computed BAR value here |
| `0x01F020D0` | 32 | CHIF doorbell/control 0 (stored into each channel descriptor `+0x08`) | `ldr r7,[..]=0x1f020d0` |
| `0x01F020D4` | 32 | CHIF control 1 — init writes **`0x04000000`** (enable); paired status halfword at `0x01F020B2` (`[reg−0x22]`, `tst #1`) | chif init |
| `0x01F03004` | 16 | **doorbell / message-pending status** — RX path reads this halfword, **bit2 (`0x4`) = incoming message pending** | rx-path `ldrh; tst #0x4` |

Two host-interface sub-blocks are in play: `0x01F02000` (CHIF control + PCI BAR, see `docs/06`)
and `0x01F03000` (doorbell/status). The ChifLite pre-DRAM variant uses the same regs.

## BMC channel descriptor — 0x4C (76) bytes, array of **10** @ RAM `0x62A7C`

Allocated per logical CHIF channel; looked up by channel id.

| Off | Type | Field (inferred) |
|-----|------|------------------|
| `+0x04` | u32 | channel state — cleared (`0`) on open |
| `+0x08`..`+0x27` | 0x20 B | channel config block — copied in from the CCB at open (`mov r2,#0x20` memcpy) |
| `+0x28` | u16 | **channel id / handle** — `0xFFFF` = free; matched against the inbound message's channel id |
| `+0x2A`..`+0x4B` | — | ring/sequence state (not fully decoded) |

**Channel-id numbering** (`chif_message_dispatch` `0x150cac`): ids are **host-assigned** — the
dispatch reads header `(subtype@+0x04, chan-id@+0x06)`, matches a descriptor by `+0x28`, calls its
handler `[desc+4]`. BMC-reserved ids: **`0xFFFE` = echo/loopback**, **`0xFFFF` = free**. Per-service
numbering follows the host hpilo convention, not a fixed BMC table. See `docs/28`.

Open/lookup walks all 10 descriptors comparing `+0x28` (`mla rX,chan,#0x4c,base`; `ldrh [rX,#0x28]`).

## Shared-memory CCB (Command Control Block) — 0x80 (128) bytes, array of **32**

Lives in the shared-memory window (the 1 MB PCI BAR). Base = `*(0x62A54)` (physical, from kernel).

| Off | Type | Field (inferred) |
|-----|------|------------------|
| `+0x04` | u32 | **message offset** into the shared window (`msg = shmem_base + offset`) |
| `+0x10` | u32 | ring control — **bit31 = ownership** (producer/consumer handoff) |
| `+0x28` | u32 | ring control — **bit31 = ownership** |

Init (`FUN_0x1500f0`) loops all 32 entries (`+ chan*0x80`) clearing bit31 of `+0x10` and `+0x28`.

## Message framing

**Magic + checksum** (`0x151484`): the global control header at `shmem_base + 0x4000` starts with
a 4-byte magic — **`"CHIF"` (`0x46494843`, msg type `0x18`)** or **`"CHI5"` (`0x35494843`, msg type
`0x19` = ChifLite/v5)** — then a **14-byte checksum** (sum `& 0xff == 0`). Per-message header:
**length @ +0x00**, **channel-id @ +0x06**. Descriptor `+0x2A` = channel **buffer size** (max
payload = `+0x2A − 0xC`). Flow control = **CCB ownership bit31** (no head/tail indices); RX copies
into a per-service **software queue (depth 4)**. See `docs/28` §3a.


`msg = shmem_base + CCB.offset`; a `0x4000` (16 KB) data region, then a header at `msg+0x4000`
whose **`+0x06` = u16 channel id** (matched to descriptor `+0x28`). Message size is bounded-
checked against `0x4000`.

## RX flow (BMC receives from host)

1. Poll/IRQ on `0x01F03004`; **bit2** set ⇒ message pending (else return).
2. Fetch the active CCB → `+0x04` offset ⇒ `msg = shmem_base + offset`.
3. Read channel id at `msg+0x4000+0x06`.
4. Linear-scan the 10 descriptors for `desc.+0x28 == channel_id`; dispatch to that channel's
   handler (`chif_handler` task, entry `0x12a74`; vUSB/VAS/etc. ride these channels).

## For the OpenBMC port

To make a Gromit-XE board talk to its host, OpenBMC must implement this **BMC-side CHIF server**:
map the shared-memory BAR window, service the `0x01F03004` doorbell, and move
CCB-referenced messages between the 10 channels and OpenBMC services (IPMI/KCS, host-power,
vMedia). This is net-new (no upstream equivalent) and is the gating work for vKVM/vMedia.
The host side is already done upstream (`hpilo`) — useful as the protocol reference/oracle.

## Open / not fully decoded

- Descriptor bytes `+0x2A..+0x4B` (send/recv ring head/tail/size).
- CCB fields other than `+0x04/+0x10/+0x28`; exact producer/consumer index semantics.
- ~~The BMC→host doorbell write~~ **RESOLVED**: write **`(1 << channel)` to `0x01F020D4`**
  (`0x1532b8`) = notify host of a message on channel N (same reg as the `0x04000000` enable).
  Host→BMC = `0x01F03004` bit2 read. Dispatch `FUN_0x152fe0` (types 0x18/0x19/0x1A). See `docs/28`.
- Physical bases (these are OS-virtual; VA→PA still pending, see `docs/05`).
