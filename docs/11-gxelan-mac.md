# iLO 3 GXELAN Ethernet MAC — register map

Date: 2026-10-20. Static RE of the type-2 **kernel/BSP** image
(`decompiled/kernel_type2.bin`, `src\umac.c`, `ARM:LE:32:v5`). GXELAN = the Gromit-XE
integrated Ethernet. All addresses are **physical** (identity-mapped device region, see
`docs/10`).

## Topology — two NICs (confirmed against hardware)

The iLO 3 exposes **two network paths**, matching the two GXELAN MACs:

| MAC | Base | NIC | PHY | Firmware |
|-----|------|-----|-----|----------|
| **MAC0** | `0xC0004000` | **Dedicated / physical** iLO management port | **dedicated PHY** (MDIO) | `iLO will use the dedicated PHY` |
| **MAC1** | `0xC0005000` | **Shared / sideband** with the host LOM | **sideband PHY / NC-SI** | `iLO will use the sideband PHY`, `bcncsi:` (NC-SI) |

The two MAC register blocks are **0x1000 apart** (probe checks `base − 0xC0005000 == 0` or
`== −0x1000`; otherwise `Unknown MAC at base=%08lx`). iLO selects dedicated vs shared at config
time; the shared path rides NC-SI/BCN-CSI to the server's onboard NIC.

## Register map

| Address | Width | Register (inferred) |
|---------|-------|---------------------|
| `0xC0004000` | — | **MAC0** register block (dedicated NIC) base |
| `0xC0005000` | — | **MAC1** register block (sideband/shared NIC) base, +0x1000 stride |
| **`0xC0004080`** | 32 | **MDIO command** — `reg[4:0] \| (phy_addr[4:0] << 16)`; set **bit9 (`0x200`)** to trigger. Shared MDIO for both PHYs. |
| **`0xC0004084`** | 32 | **MDIO write data** (read data returned here after trigger) |
| **`0xC00000C0`** | 16 | **GXELAN global reset / control** (in the ASIC *core* block, not the MAC block). Reset pulse: set bit (`0x04` or `0x80`), delay, restore. Matches app symbol `RST_GXELAN_L`. |

MDIO sequence (from `umac` phy read/write): write command (`0xC0004080`), write data
(`0xC0004084`), set trigger bit9, poll for completion.

## DMA / descriptors

- **32-entry** TX/RX descriptor rings (probe loops `0..0x1F`), managed as software structures in
  kernel RAM (`0x20xxxxxx`, the type-2 kernel data region) and handed to the MAC via DMA.
- Per-ring descriptor pointer / status registers live in the MAC block (`0xC0004000` + offset) —
  accessed off a base parameter, so exact offsets beyond MDIO (`+0x80/+0x84`) are not yet
  enumerated. `%d norxdesc packets`, `no tx descriptors errors: check clocking`.
- **Ethernet CRC-32**, poly `0x04C11DB6` (reflected `0x04C11DB7`) — hardware/software MAC-address
  hashing for the multicast filter.

## Config / addressing

- Two MAC addresses (`No MAC Address 0/1`, broadcast/multicast/dummy slots per MAC), read from
  **NVRAM** via the `netcfg` IODevice (`umacdev: NVRAM Read failed`, `Unable to get netcfg
  IODevice`). `Illegal MAC Address combination` / `Both MAC Addresses are the same` are sanity
  checks.
- Link status: `%s[%d]: Link: UP %d Mbps/Gbps %s-Duplex` (10/100/1000).
- Loopback/diag: `umacdev_loopback_save` (speed/duplex/isolate via MDIO).

## For the OpenBMC port

GXELAN needs a **custom MAC driver** (no upstream `hpe,gxelan` net driver exists). Two
`ethernet` DT nodes:
- `ethernet@c0004000` — dedicated iLO NIC (its own PHY on the shared MDIO).
- `ethernet@c0005000` — sideband/shared NIC; the shared path is **NC-SI**, so model it with the
  kernel `ncsi` stack (OpenBMC `phosphor-networkd` + NC-SI) rather than a direct PHY.
Shared MDIO bus node at `0xC0004080`; global reset bit in the syscon at `0xC00000C0`.

## Open

- Full MAC0/MAC1 register offsets beyond MDIO (control, status, descriptor-base, interrupt
  mask) — accessed via a base parameter; disassemble `umac` init/start to enumerate.
- Interrupt line(s) for the MACs (interrupt-controller mapping still TODO project-wide).

## Register map — RE pass 2 (disassembly-confirmed)

Second pass over `umac.c` in the kernel (reset fn `0x2d44c`, addr/hash program `0x2d59c`/
`0x2d578`, MDIO `0x2c800`). All CONFIRMED from disassembly.

### MDIO (shared bus, both PHYs) — CONFIRMED

| Reg | Field | Role |
|-----|-------|------|
| `0xC0004080` CMD | bits[4:0] | PHY register address |
|  | bits[20:16] | PHY address |
|  | **bit8 (0x100)** | direction: **set = read**, clear = write |
|  | **bit9 (0x200)** | **trigger/busy** — set to start; poll to 0 = done |
| `0xC0004084` DATA | [15:0] | write data (write op) / read data (read op) |

- **Read**: `CMD = reg | (phy<<16) | 0x100`; write CMD; `CMD |= 0x200`; poll `CMD & 0x200 == 0`; read `DATA`.
- **Write**: `CMD = reg | (phy<<16)` (no 0x100); write `DATA`; write CMD; `CMD |= 0x200`; poll.

### MAC block registers (offset from base 0xC0004000 / 0xC0005000) — CONFIRMED

| Off | W | Role |
|-----|---|------|
| `+0x14` | 16 | **control/command** (written frequently; enable/kick — exact bits best-reading) |
| `+0x18` | 16 | station MAC address bytes [0:1] (big-endian in the halfword) |
| `+0x1C` | 16 | station MAC address bytes [2:3] |
| `+0x20` | 16 | station MAC address bytes [4:5] |
| `+0x24` | 32 | multicast hash filter word 0 |
| `+0x28` | 32 | multicast hash filter word 1 |

- Multicast filter: CRC-32 (poly `0x04C11DB6`) of each 6-byte address → 6-bit index → set a bit in
  the 64-bit hash (`+0x24`/`+0x28`). Address program packs the 6 bytes into `+0x18/+0x1C/+0x20`.

### Reset (ASIC core block) — CONFIRMED

`0xC00000C0` (16-bit): pulse **bit 0x04 = MAC0 (dedicated)**, **bit 0x80 = MAC1 (sideband)** —
set, delay ~10, restore. Probe identifies the MAC by `base == 0xC0005000` (MAC1) or
`base == 0xC0004000` (MAC0), else `Unknown MAC at base=%08lx`.

### TX/RX DMA — NOT statically recoverable (IODevice-abstracted)

The TX/RX rings are managed as **INTEGRITY DMA descriptor objects** (software descriptors with
method pointers — the TX path `0x2d608` loops 32 descriptors and calls `blx [desc+8]`/`[desc+0xc]`,
not hardware ring registers). The hardware ring-base / head / tail registers and the on-wire
descriptor format live below this abstraction in the kernel DMA layer and are **not recoverable
from static RE** — they must be pinned during hardware bring-up. IRQ 16 (MAC0) / 17 (MAC1), docs/12.

## RE update (2026-10-21): DMA ring confirmed object-mediated (datapath blocked)

Disassembled the `umac` driver in `decompiled/kernel_type2.bin` (realigned, base 0) to try to
recover the TX/RX ring descriptor base registers. Findings:

- **MAC base pointer = driver struct `+0x04`.** Station-addr writer (`0x2d59c`): reads the base
  from `[struct+4]`, writes the 6 address bytes to `[base+0x18/+0x1c/+0x20]` — confirms docs/11's
  ADDR0/1/2. Hash flush (`0x2d578`): `[base+0x24] = struct+0x15c`, `[base+0x28] = struct+0x158`.
- **The open/init path (`0x2d604`) does NOT write any ring-base MMIO.** It is a 0..31
  registration loop that allocates/attaches per-index objects (`bl 0x2caec`, result at
  `[struct+0xc]`) and drives them through **INTEGRITY DMA object method pointers**
  (`ldr r1,[obj+8]; blx r1` and `ldr r1,[obj+0xc]; blx r1`). The TX/RX descriptor ring base,
  head/tail and descriptor layout live *inside* those generic DMA-object methods, not as MAC
  register literals in the driver.

**Conclusion: the ring format is genuinely not statically recoverable from the driver** (confirms
the original docs/11 note). Recovering it needs either on-hardware reconstruction (observe the
descriptor memory + MAC registers on a running board) or a Gromit-XE MAC datasheet. **Phase 3
networking is therefore HW-gated at the MAC datapath**: the Rust side builds the full smoltcp stack
behind a `MacDriver` ring seam (`firmware/src/net.rs`), ready to drop the real ring in once pinned
on hardware. MDIO + MAC config (reset/addr/hash) are usable now (`firmware/src/mac.rs`).
