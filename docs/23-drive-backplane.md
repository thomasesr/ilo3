# iLO 3 drive backplane (SEP) — product ID, UID LED, health over I2C

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`src\drives.c`,
`ARM:LE:32:v5`, base 0; code addr = file offset). Command frames CONFIRMED by disassembly.

## Transport — one I2C RPC, no drive MMIO/SGPIO registers

All backplane access routes through **`0x136cd0`** = an I2C transaction RPC to the INTEGRITY
`"I2CService"` (dispatch `0x14ab9c`). There are **no MMIO/SGPIO hardware registers** in the
drive path (zero `0x01Fxxxxx`/`0xC00xxxxx` literals in the whole `drives.c` code range) — the
"SGPIO" data is the SEP's own SGPIO state surfaced through its I2C command port.
Args: `r0`=I2C engine/bus (0-7), `r1`=8-bit slave addr, `r2`=write len, `r3`=write buf, +stack:
read-len, read-buf. Write-only ops pass read-len 0.

## SEP address + discovery (CONFIRMED)

- **SEP slave address = 8-bit `0xE0,0xE2,…,0xEE` (7-bit `0x70-0x77`)**, one per backplane.
- `drives_get_backplane_i2c_addr` **`0x132160`**: ACK-probes (zero-length write) from `0xE0`,
  `addr += 2`, while `< 0xEF`; first ACK = that SEP's address.
- **Cache table at `[0x8b8d8]`**: up to 8 records × `0x6C` bytes: `+0x00` valid, `+0x01` SEP
  addr, `+0x02` engine/bus, `+0x10` = 4 bay sub-entries × 24 B (`+0` bay id, `+3` LED bit-pos).
- addr→bus: `0x133244`; bay→SEP: `htdrv_get_drive_bay_i2c` `0x1332b0`.

## SEP command set (byte frame: `[op] 00 [len] [cnt] [cmd]…`)

| Function | Addr | Frame (write) | Reply |
|----------|------|---------------|-------|
| **UID/locate LED set** | `0x132548` | `D8 00 07 06 <uid_idx> <chk>` | write-only |
| **Status / LED read** (health) | `0x1322f8` | `D2 00 05 05 24` | status/LED bytes |
| **SGPIO read** (presence/locate/fault) | `0x13274c` | `D2 00 06 05 23` | 5 B: `50 b1 b2 b3 b4`, checksum `~(b1+b2+b3+b4+0x50)` |

`0xD8`=SEP write opcode, `0xD2`=SEP read opcode (NOT the slave address). All three frames
CONFIRMED by disassembly.

- **UID LED set** `drive_set_drive_uid_state` → `htdrv_set_drive_uid` (`0x131d44`) resolves the
  bay SEP, reads current UID (`0x132f74`), OR/ANDs the per-bay LED bit, → `drives_set_uids`
  (`0x1325ec`) → `0x132548`. `htdrv_set_drive_uid … i2c_addr: %x`.
- **Health/status** `drives_led_get` `0x1322f8` → callers decode `driveLedStatus`/`driveUIDState`.
- **SGPIO** `drives_get_sgpio_data` `0x13274c`: validates header `0x50` + checksum, bit-extracts
  presence/locate/fault from reply bytes `+0x1a/+0x1b` (masks `0x20`/`0x10`).

## Drive product ID / model — NOT from the SEP (inferred)

The SEP only returns ≤5 status bytes; no ASCII crosses backplane I2C. Model strings come from a
**global inventory table `[0xb6345]`** (88 × 0x1C B; model = 15-byte string at `entry+0xC`,
default `"N/A"`), keyed by 16-bit drive id — displayed by `Drive productID[%d]: %s`,
`ProductID: %s`, RIBCL `PRODUCT_ID VALUE="%s"`. This table is **host-sourced** (Smart Array /
drive inventory delivered via CHIF/host-agent), not the backplane. The exact CHIF producer was
not traced (strongly supported — no I2C string read exists).

## OpenBMC port mapping

- **I2CService RPC → `i2c-gxe` adapter** (docs/16): the SEP is an I2C client at `0x70-0x77` on
  its engine bus.
- **UID/presence/fault LEDs**: the SEP speaks a **proprietary HP command set** (`D2/D8 …`), not
  SAF-TE/SES — so a port needs a small **userspace shim emitting these exact frames**, exposed as
  `xyz.openbmc_project.Led.Group` (locate) + `Inventory.Item.Drive` (presence/health) — NOT the
  kernel SGPIO/`sgpio-aspeed` block (this HW doesn't use an iLO SGPIO controller for drives).
- **Drive model/serial**: comes from the storage-controller/host path (CHIF/IPMI/Redfish) →
  `Inventory.Item.Drive` `Model`/`SerialNumber`, mirroring the `[0xb6345]` cache — not the SEP.
- **Backplane discovery**: the `0xE0→0xEE` ACK-probe → entity-manager I2C probe entries / bus scan.
