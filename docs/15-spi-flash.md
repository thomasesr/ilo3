# iLO 3 SPI boot flash — SMIF controller `0xC0000200`

Date: 2026-10-20. Static RE of the type-2 **kernel** (`decompiled/kernel_type2.bin`,
`gxekflsh` SPI driver, `ARM:LE:32:v5`). This maps how the BMC drives its own boot SPI flash
(the `ilo3_194.bin` image lives here).

## Correction to `docs/13`

The `0xF0000000`/`0xF8000000` windows are **not** the SPI-flash XIP window — the SPI flash is
driven by a **register-based controller**, not memory-mapped. `0xF8000000` appears only as a
kernel memory-region *boundary* (relocation math); `0xF0000000`–`0xF7FFFFFF` is a large,
heavily-used memory region (186 app accessors) whose exact role (BMC DRAM/heap vs host-memory
window) is **not yet resolved**. See "Open" below.

## SPI flash controller (SMIF / `gxekflsh`) — base `0xC0000200`

The low-level SPI flash interface is in the ASIC core block at **`0xC0000200`** (`gxekflsh_*`
in the kernel). It is a command/status register file, not an address-mapped flash. From
`gxekflsh_spi_is_busy` (issues an SPI **RDSR** and reads the WIP bit):

| Reg | Access | Role (inferred) |
|-----|--------|------------------|
| `+0x00` (`0xC0000200`) | strb | control / go-low |
| `+0x02` (`0xC0000202`) | ldrb/strb | **status/control** (bit1 polled; write `1` to start a transfer) |
| `+0x03` (`0xC0000203`) | strb | **SPI command / unlock** — unlock seq `0x55`,`0x55→0xAA`; opcode e.g. `0x05` (RDSR) |
| `+0x04` (`0xC0000204`) | strb | transfer length / count |
| `+0x3E` (`0xC000023E`) | ldrb | **result / read-data** (RDSR result; **bit0 = WIP/busy**) |

Sequence (RDSR busy-check): unlock (`0x55`/`0xAA` → `+0x03`), clear/setup (`+0x02`,`+0x04`,`+0x00`),
write opcode `0x05` → `+0x03`, start (`+0x02 = 1`), wait, read `+0x3E` bit0 = flash busy.

## Flash subsystem (kernel)

- **`gxekflsh`** = "GXE Kernel Flash" — the SPI flash HAL (read/erase/program).
- **`.mr___flashbuf`** — a dedicated flash staging buffer memory region.
- **`.lastsectorremap`** — bad/last-sector **remap** (wear/defect handling).
- Flash descriptor **magic `0x10ADBEEF`** (validates flash structures; `Flash info is corrupted`).
- Firmware update: `226-Flashing %3u%% complete`, `226-Flashing completed`, `PASS flash`,
  `CRC error in FLASH, failing`. Inventory: `kftp: inventory SPI`.

## Two SPI paths — iLO flash vs host System ROM

- **`0xC0000200` (`gxekflsh`)** = the **iLO's own boot flash** (this firmware image).
- **`0x01F10000` (`smifpkt`/`romchfservice`)** — labelled "SMIF" in `docs/05` — is more likely
  the **host System ROM (BIOS) access** path (iLO can read/flash the server's BIOS over the
  host-interface/CHIF). Keep these distinct; confirm by tracing `romchfservice_c.c`.

## For the OpenBMC port

- iLO boot flash → an **SPI-NOR** controller driver for the Gromit-XE SMIF at `0xC0000200`
  (custom `spi-mem`/`mtd` driver; no upstream `hpe,gxe-smif`). Partition the flash per
  `docs/01`/`docs/03` (boot ROM, main image, kernel, certs/sig) for `mtd`/`fw_env`.
- The `0x55/0xAA` unlock + opcode-in-`+0x03` protocol must be reproduced by the new driver.

## Open

- ~~Map the remaining SMIF registers (address, data-FIFO, erase/program opcodes).~~ **DONE → `docs/17`** (full protocol: MODE +0x02, GO/BUSY +0x04, OPCODE +0x05, COUNT +0x06, ADDR +0x08, FIFO +0x40; opcodes WREN/RDSR/PP 0x02/erase 0xD8).
- Resolve the `0xF0000000`–`0xF7FFFFFF` (128 MB) region's role — BMC DRAM/heap vs host-memory
  access window (186 app accessors; not flash).
- Confirm the `0x01F10000` "SMIF" is host-BIOS access (`romchfservice`), separate from the iLO
  boot flash controller here.
