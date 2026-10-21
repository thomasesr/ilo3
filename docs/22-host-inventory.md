# iLO 3 host inventory — processor & memory discovery

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`ARM:LE:32:v5`, base 0;
code addr = file offset). Verified by disassembly.

## Source: host BIOS SMBIOS/DMI over ROM-CHIF

**Both CPU and memory inventory (presence, model/name, speed, per-slot size/type) come
from the host BIOS SMBIOS table**, streamed to iLO over the **ROM-CHIF** channel
(`romchf_service`, `src\romchfservice_c.c`) and cached. PECI and SPD-over-SMBus exist in
parallel but feed **thermal/health**, not the authoritative inventory — same design iLO
keeps throughout ("ROM type N" records).

- SMBIOS delivered over the **host-interface block `0x01F02000`** (SMBIOS pointer `+0xA0`,
  docs/06) / CHIF shared memory; host raises `EVT_SMBIOS_RCVD` when the table arrives.
- **Record fetcher `0x8cff4c`**: builds a request (opcode **3** = get-record-by-type+handle;
  type at struct `+0x124`, handle `+0x128`) and issues `VComClientSync_Call` to the service
  **`romchf_service`** (string @ `0xe8da0`). Handle-chase variant `0x8d00b4` (opcode 5).
  Generic accessor `smbios_get_rec_by_type_and_position`.

## Processor (CONFIRMED)

- **`smbios_get_proc_info` @ `0x8d100c`** — fetches **SMBIOS type 4** (Processor Information):
  `mov r0,#4; bl 0x8cff4c` @ `0x8d105c`, then **type 7** (Cache) `mov r0,#7` @ `0x8d10f4`.
  Extracts socket designation (50-byte strncpy), model-name string (SMBIOS string-set walker
  `0x8cfd24`), speed/core bytes. 256-byte record buffer (frame `0x208`).
- Evidence: `Record4: smbios_get_proc_info returned rc %d.` @ `0x8a8f98`; builder calls it at
  `0x8a8cf8`. `Couldn't get the SMBIOS type 4 record at handle %04X` @ 4539764.

## Memory (CONFIRMED)

- **`smbios_get_mem_info` @ `0x8d143c`** — fetches **SMBIOS type 17** (Memory Device) per slot:
  `mov r0,#0x11; bl 0x8cff4c` @ `0x8d14b0`. Extracts device + bank locator strings, size
  (`ldrh`), device-type byte → per-slot population/size/type/speed.
- Evidence: `Record17: smbios_get_mem_info returned rc %d.` @ `0x8a93c8`; builder at `0x8a90e0`.
  `SMBIOS DIMM lists all DIMM positions supported by the host` @ 1230811.

## Secondary paths (NOT the inventory source)

- **PECI** (`src\peci.c`, `run_peci_cmd`, IODevice `IODPECI`, base CPLD `0x01F14068` IRQ 60):
  CPU **DTS temperature** (`PECI DTS - CPU %u`), machine-check decode, CPU PCI-config reads
  (`peci_read_pciconfig`). `XNC NOT ready - failed PECI MUX target` = multi-node (XNC) boxes.
- **SPD-over-SMBus** (peci.c `peci_read_one_DIMM_register`, PCA9542 mux `SMB command to select
  9542 Mux channel`): raw DIMM SPD bytes for **DIMM thermal + SPD-CRC logging** (`dimm_thread`,
  `eh_intf_log_spd_data`), not size/type/speed.
- **BMC-side SMBIOS updater threads** (`peci_smbios_thr` "PECI SMBIOS update monitor" ~`0x11c330`
  loops 64 DIMM slots; `i2c_smbios_thread` "i2c smbios sync" creator `0xe3d80`) — supplement/sync.
- **`memid` scan-chain** (`memid: %02x`, `eh_desc: MEMID`): CPLD-driven memory-**board/cartridge**
  presence ID (cartridge G7 memory boards), not per-DIMM data.

## OpenBMC port mapping

- **Processor** → ingest host SMBIOS over the host interface and publish via **smbios-mdr**
  (MDRv2) → Redfish `Processors`; live CPU temp via mainline **PECI** stack (not entity-manager
  hardcode). The SMBIOS-over-CHIF intake = net-new MDRv2 transfer agent for the GXE host iface.
- **Memory** → host SMBIOS **type 17** → **smbios-mdr** → Redfish `Memory`. Optional BMC-native
  DIMM presence/temp via Linux **SPD EEPROM** (`ee1004`/`jc42`) through the DIMM SMBus mux, but
  treat host SMBIOS as authoritative (as iLO 3 does).

| Datum | Function | Addr | Source |
|-------|----------|------|--------|
| CPU presence/model/speed/cache | `smbios_get_proc_info` | `0x8d100c` | SMBIOS type 4(+7) via romchf |
| SMBIOS record fetch | fetcher | `0x8cff4c` | VComClientSync → `romchf_service` |
| DIMM population/size/type/locator | `smbios_get_mem_info` | `0x8d143c` | SMBIOS type 17 via romchf |
| DIMM SPD raw (thermal/CRC) | `peci_read_one_DIMM_register` | peci.c | SMBus via 9542 mux |
| CPU DTS temp (health) | `run_peci_cmd` | peci.c | PECI, CPLD `0x01F14068` |
