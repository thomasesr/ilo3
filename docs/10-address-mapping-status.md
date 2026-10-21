# iLO 3 address mapping (VA↔PA) — RESOLVED: device regions are identity-mapped

Date: 2026-10-20 (supersedes the earlier "blocked" draft). The peripheral addresses in
`docs/05`–`docs/09` are **physical** — device regions are identity-mapped (VA = PA).

## The kernel was hiding in plain sight: the type-2 image is the INTEGRITY kernel

`docs/03` listed the two **type-2** flash images (`0x6f0000`, `0x770000`, build **v0.8.42**,
795 KB) as "NIC/`umac.c` data". **That was wrong — they are the INTEGRITY kernel + BSP**
(separately versioned from the v1.94.2 application). They are **dense phase-1 ARM code** (3341
STMFD + 3275 APCS prologues; I had not realigned them). Realigned copy:
`decompiled/kernel_type2.bin`.

Evidence it is the kernel/HAL:
- **MMU control block** at type-2 offset `~0x0af600–0x0aff10`: `MRC/MCR p15,c1` (SCTLR —
  enable/disable MMU), `MCR p15,c3` (DACR), `MCR p15,c8,c7,0` (TLBIALL), `MCR p15,c7,c10,4`
  (DSB), and the **TTBR0 write `MCR p15,0,r0,c2,c0,0` at `0x0aff0c`**.
- **ARM926 FCSE** (Fast Context Switch Extension): nine `MCR p15,0,Rt,c13,c0,0`
  (FCSE PID / Context-ID) writes — this is how each INTEGRITY AddressSpace gets its own
  0–32 MB window, and why app code calls "kernel" addresses at `0x018xxxxx` (another process's
  window), with ~2846 cross-space call targets spanning `0x01800000–0x02000000`.

## Why the device addresses are physical (identity-mapped)

The kernel references, as direct literals in the **same** MMU-enabled image:
- `0xC0000097`, `0xC0000098`, `0xC00000A0`, `0xC0000064`, `0xC000004C`, `0xCEFF00xx` — the
  **RTC / sys / CEFF** block the boot ROM proved is **physical `0xC0000000`**; and
- `0x01F14000` — the CPLD block (`docs/09`).

Both ranges are accessed directly, with MMU on → both are **mapped**, and `0xC0000098` maps to
itself (identity, = the boot-ROM physical RTC). So the ASIC's two device windows are
**identity-mapped**:

| Virtual (OS) | Physical | Contents |
|--------------|----------|----------|
| `0xC0000000` | `0xC0000000` | ASIC core: system ctrl, crypto `+0x0C`, **RTC** `+0x97`, **UART** `+0xF0` |
| `0x01F00000` | `0x01F00000` | peripheral window: fan/PWM, SMBIOS, KCS+WDT, **SMIF**, **I2C**, **CPLD/PWRMICRO**, CHIF |

(The app uses only the `0x01F00000` window; the kernel uses both. They are two *separate*
physical device regions, not aliases — the kernel uses both literal ranges side by side.)

## Consequence for the devicetree — all `reg` values stand as physical

Every address in `docs/05`–`docs/09` is a **physical** base usable directly in the OpenBMC DT:
UART `0xC00000F0`, RTC `0xC0000097`, I2C `0x01F10100`, SMIF `0x01F10000`, KCS `0x01F0D400`,
CHIF `0x01F02000`, CPLD/PWRMICRO `0x01F14000`, PECI in the CPLD block, etc. No translation
needed. FCSE/Context-ID affects only per-process code/data in low RAM, not these device bases.

## Residual / nice-to-have

- Confirm the L1 section descriptor for VA `0x01F00000` is literally identity (the page table is
  runtime-built by the type-2 kernel near `0x0aff0c`); the direct-literal evidence above is
  already strong.
- The type-2 kernel also contains the **network MAC driver** (`umac.c`), the MMU/HAL, and the
  IODevice registry — a good target for further BSP RE (GXELAN MAC, interrupt controller).
