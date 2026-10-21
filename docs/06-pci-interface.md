# iLO 3 host PCI interface (Gromit-XE as a multifunction PCI endpoint)

Date: 2026-10-20. Source: host `lspci` + static RE of `decompiled/os_main_base0.bin`
(`ARM:LE:32:v5`, base 0). The Gromit-XE ASIC presents itself to the **host x86** as a
multifunction PCI endpoint; this maps those functions to the BMC-side firmware that implements
them.

## Host view (verified against `lspci -vvv` on a live G7 host)

```
02:00.0 System peripheral [0880]  103c:3306  Slave Instrumentation & System Support   (rev 04)
02:00.2 System peripheral [0880]  103c:3307  Management Processor Support & Messaging (rev 04)
02:00.4 USB controller   [0c03]  103c:3300  Virtual USB Controller  prog-if 00 [UHCI] (rev 01)
```
Vendor `0x103C` = HP. **Linux already drives the host side**: `drivers/misc/hpilo.c` binds
`103c:3307` (CHIF); `uhci_hcd` binds `103c:3300` (.4). This doc is the *BMC* side.

PCIe: all three are **PCIe v1 Legacy Endpoint, Gen1 2.5 GT/s × 1**, MaxPayload 128 B, MSI
capability present but **disabled → INTx** used. Subsystem IDs: `.0` = `103c:3309`, `.2` =
`103c:`"iLO 2", `.4` = `103c:`**"iLO2 GXL / iLO3 GXE"** — the subsystem string itself names the
ASIC: iLO2 = Gromit-XL, **iLO3 = Gromit-XE** (matches `docs/02`).

### Verified BAR / IRQ map (host-assigned addresses on this box)

| Fn | IRQ (pin) | I/O BARs | Memory BARs | Exp ROM |
|----|-----------|----------|-------------|---------|
| `.0` 3306 | 11 (A) | R0 `0x3000`/256, R2 `0x3400`/256 | R1 `f1ff0000`/**512 B** | — |
| `.2` 3307 | 17 (B) | R0 `0x3800`/256 | R1 `f1fe0000`/256, **R2 `f1e00000`/1 MB (CHIF shared-mem)**, R3 `f1d80000`/512 K, R4 `f1d70000`/32 K, R5 `f1d60000`/32 K | `f1d00000` **64 K** |
| `.4` 3300 | 17 (B, shared) | R4 `0x3c00`/**32** (UHCI I/O regs) | — | — |

Firmware cross-checks that **confirm** this hardware: the embedded PCI option ROM is **exactly
64 K** (PCIR img-len `0x80`×512) for device `3307` tagged `HP ILO RBSU` → matches the 64 K
Expansion ROM on `.2`; the CHIF code programs a ~1 MB shared-memory BAR → matches `.2` R2 (1 MB).

## Function → BMC implementation map

| Fn | ID | Role | BMC-side firmware | Registers (VA, `0x01F02000` block) |
|----|----|------|-------------------|-------------------------------------|
| `.0` | `103c:3306` | Slave Instrumentation / **VAS / ISS** (iLO Standard Slave): host power, health, SMBIOS | `src\rbsu.c`, `hostpwr_*` | `+0x46` host-power (HOSTPWR), `+0x54` VAS/RBSU(ISS), `+0x70` host-power monitor, `+0xA0` SMBIOS |
| `.2` | `103c:3307` | Management messaging — **CHIF** (Channel InterFace) | `src\chif.c`, `chif_handler` | `+0x50` CHIF channel/BAR, `+0xD0`/`+0xD4` CHIF/ChifLite |
| `.4` | `103c:3300` | **Virtual USB — UHCI (USB 1.1)** — vMedia/kbd/mouse | `src\uhci.c` (ctrl) + `src\eh_intf.c`/`usbservice_c.c` (bridge) | host-facing UHCI I/O regs via BAR `0x3c00`; device side rides CHIF (`eh_intf_submit → chif_post`) |

## CHIF — the shared-memory transport (core of the interface)

`src\chif.c` init (`FUN_0x14fc44`) is the heart of the host interface:

- Obtains a **physical shared-memory region** from the INTEGRITY kernel, maps it
  (`chif: Unable to map the required physical addresses` / `… get physical address of shared
  memory` are its failure paths). Shared-mem base is cached in RAM global `0x62a54`.
- Programs the **PCI BAR register at `0x01F02050`** with the window: it computes
  `(phys − 0x40000000)` OR'd with `log2(size)` and stores it — i.e. the host-visible BAR maps
  BMC RAM (in the `~0x40000000` physical region) into the host's address space.
- Builds an array of **0x4C-byte channel descriptors** over `0x1000`-byte (4 KB) pages;
  base/base+0x1000 pairs → per-channel ring buffers.
- Depends on **SMIF** (`chif: smif started successfully` / `[FATAL] failed to start smif!`) and
  the **sawbase** shared-memory window manager (`src`-level `sawbase*`).
- On success: `chif: the channel is open for business.`
- `ChifLite` is a cut-down pre-memory-init variant (`chiflite_premem`, used before DRAM is up).

All three PCI functions are logical channels multiplexed over this one CHIF shared-memory
transport + a doorbell/interrupt. vUSB: the host sees a real **UHCI** controller (driver `src\uhci.c`, IODevices `IODUHCIHC` + `IODUHCIWALKER`, "Fn4 PCI-E Status Reg UHCIPCISR"); the device side = `usbservice_c.c` + the `vm_*` manager over CHIF (see **`docs/29`**). (NB: `eh_intf`→`chif_post` is the *Event Handler* health-event path, NOT vMedia.) Keyboard+mouse are always-connected; media attaches dynamically.

## PCI option ROM

One PCI expansion ROM is embedded (for the host BIOS): `AA55` signature + **`PCIR` data
structure @ `0x2ea1a6`** = vendor `0x103C`, device `0x3307`, class `0x088000`, with the ASCII
tag **`HP ILO RBSU`** (ROM-Based Setup Utility / ISS). Only the `.2`/`3307` function carries one
here; `.0`/`.4` use ASIC-default config.

## Relevance to the OpenBMC port

- The PCI functions are **host-facing** — they are how the server's BIOS/OS talks to the BMC
  (IPMI-over-PCI/KCS, health, virtual media). An OpenBMC port must either reuse the Gromit-XE
  host-interface block (`0x01F02000` + CHIF shared memory) or bridge it to standard OpenBMC host
  interfaces (KCS at `0x01F0D400` — see `docs/05` — maps to `phosphor-ipmi-kcs`).
- Mainline Linux `hpilo` is the *host* client; there is **no BMC-side** CHIF/VAS/vUSB server in
  OpenBMC — that is net-new work, the hardest part of a functional port (vKVM/vMedia).

## Open / next

- Resolve the `0x01F02000` block to a **physical** base (VA→PA trace, as in `docs/05`) for a DT
  node; it is the Gromit-XE "host interface" / PCIe-endpoint controller.
- Map the CHIF **doorbell/interrupt** register and the channel-descriptor format (0x4C bytes).
- Decode the per-function PCI config (BARs/sizes) the ASIC presents — partially in the option
  ROM, rest likely ASIC straps/defaults.
