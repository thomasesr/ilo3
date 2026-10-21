# iLO 3 ↔ BIOS (System ROM) interface — consolidated

Date: 2026-10-21. Synthesis of the host-facing RE (`docs/06`,`22`,`24`,`28`,`29`,`43`) + this
pass. How the iLO 3 management processor talks to the server's **System ROM (BIOS)** and host.
Relevant to the Rust rewrite's host-interface layer.

## Physical basis — Gromit-XE is a multifunction PCI endpoint

The ASIC presents to the host (BIOS + OS) as PCI device `103c:*` (`docs/06`):
- **`.0` 3306** — **ISS / VAS / RBSU** (iLO Standard Slave, slave instrumentation, ROM-Based
  Setup Utility). Health/inventory the BIOS + host agents query; the iLO config UI inside BIOS
  setup (option ROM, `HP ILO RBSU`).
- **`.2` 3307** — **CHIF** (Channel Interface) messaging — the main shared-memory transport.
- **`.4` 3300** — **virtual USB (UHCI)** — vMedia/keyboard/mouse the BIOS can boot from (`docs/29`).

## The channels (how they actually talk)

### 1. CHIF — shared memory + doorbell (the backbone, `docs/28`)
1 MB shared-memory BAR + doorbells (RX `0x01F03004` bit2 / TX `0x01F020D4` `1<<chan`). 10 logical
channels over CCB ownership handshake + `CHIF`/`CHI5` magic. Everything below rides CHIF or a
sibling block.

### 2. ROM-CHIF — the System ROM's channel (`src\romchfservice_c.c`, `docs/22`,`43`)
The BIOS's dedicated path. Carries:
- **SMBIOS / DMI**: the BIOS **streams the SMBIOS table to iLO** → CPU/memory/NIC inventory
  (`docs/22`), `EVT_SMBIOS_RCVD`.
- **Power control** (`ROMCHIF_MSGID_*`): power-cap **BURN PID** (`SET_PID_{GAIN,KP,KD}`),
  envelope (`MAX_BURN`/`MIN_BURN`/`MIN_CAP_X`, `BURN_TEST_START`/`IDLE_TEST_START`), **CPU
  throttle** (`CPUFSTOP_NOTIFY`), `TOPOLOGY_NOTIFY`, `ASM_RESET` (`docs/43`).

### 3. IPMI / KCS (`docs/05`,`36`)
Host system interface (KCS @ `0x01F0D400`) — the OS (and BIOS) exchange IPMI (SEL, sensors,
chassis, Get Device ID) with iLO. Also surfaced to the host over PCI (`IPMIPCISR`).

### 4. POST monitoring (iLO watches the BIOS boot)
iLO **tracks the host POST**: `POST_CODE`, `POST_STATE`, `POST_IN_PROGRESS`,
`POST_PCI_CONFIGURED`, `POST_TRACKER_STATE_SAW_{POWER_ON,POST_FLAG_SET,POST_FLAG_CLEAR}`. The BIOS
emits POST progress codes; iLO reads them (via the host-interface/VAS block, `0x01F02000`) for the
**boot-progress display**, POST-code log, and **hang/ASR** detection (reset the server if POST
stalls).

### 5. ISS / VAS — slave instrumentation (`.0`, `0x01F02000` block, `docs/06`)
The BIOS/host-agent interface for health + host-power + SMBIOS: `+0x46` host-power (HOSTPWR),
`+0x54` VAS/RBSU (ISS), `+0x70` power monitor, `+0xA0` SMBIOS pointer. iLO exposes health/power
here; the BIOS and HP agents read it.

### 6. Host power / state (`docs/24`)
iLO drives the host power via the host-interface block (vPWRBTN `0x01F02046`, state `0x01F02070`,
reset/NMI/CSM) — the BIOS/host see power events; iLO sees power-good/POST state.

## Who sends what

| Direction | Content |
|-----------|---------|
| **BIOS → iLO** | SMBIOS/DMI inventory; **POST codes/progress**; power measurement (burn test); CPU topology; health/sensor data via ISS |
| **iLO → BIOS** | **power cap** (throttle to watts) + **Power Regulator** P-state mode (`docs/43`); **boot order** (one-time/persistent); **virtual media** as a boot device (`docs/29`); RBSU/iLO config; NMI/reset/power |

**Note on fans**: thermal control is the **BIOS's** job — there is **no fan/thermal command** over
ROM-CHIF; iLO only reads temps/tach + applies its fail-safe (`docs/42`).

## Rust rewrite mapping

- Implement the **CHIF server** (`docs/28`) first — the backbone. Then **ROM-CHIF** channel
  handlers: SMBIOS intake (→ inventory), POST tracker (→ boot-progress + ASR), power
  cap/regulator (`docs/43`).
- **IPMI/KCS** responder (`docs/36`) for the OS/BIOS IPMI path.
- **ISS/VAS** block (`0x01F02000`) for health/power/SMBIOS + the POST register read.
- **Option ROM / RBSU**: the iLO BIOS-setup presence (`docs/06`) — low priority; the PCI option
  ROM is HP-specific (regenerate or drop).

## Open

- Exact POST-code register in the `0x01F02000` block + the POST-tracker state machine.
- Full ROM-CHIF message framing (payloads) for SMBIOS transfer + power control.
- The ISS/VAS command set the BIOS/agents use (`+0x54`).
