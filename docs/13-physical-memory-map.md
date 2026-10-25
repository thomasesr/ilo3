# iLO 3 physical memory map (Gromit-XE, ARM926)

Date: 2026-10-20. From the type-2 **kernel** MMU region table (`decompiled/kernel_type2.bin`
@ `~0xbe840`) + the per-subsystem RE in `docs/05`–`docs/12`. All device regions are
**identity-mapped** (VA = PA; `docs/10`). Entries are `{base, limit, flags}` (flags `0x3` =
RW device/RAM, `0x1` = RO/host window).

## Low 32 MB (`0x00000000`–`0x02000000`) — per-AddressSpace (FCSE)

Each INTEGRITY process sees its own 0–32 MB via ARM926 FCSE; the kernel (PID 0) is identity.
Contains per-process code/data **and** the memory-mapped peripheral window:

| Region | Contents |
|--------|----------|
| `0x00000000`–… | process code/data (app `v1.94.2`; see `docs/04`) |
| **`0x01F00000`–`0x01FFFFFF`** | **peripheral window** — fan/PWM `0x01F00000`, SMBIOS `0x01F02000`?/host-iface, KCS+WDT `0x01F0D400`, SMIF `0x01F10000`, I2C `0x01F10100`, CPLD/PWRMICRO `0x01F14000` (`docs/05`,`09`) |

## High regions (global, from the kernel MMU table)

| Base – Limit | Size | Contents |
|--------------|------|----------|
| `0x20000000`–`0x2FFFFFFF` | 256 M | **kernel/BSP RAM** (type-2 kernel code+data load region, `0x20xxxxxx`) |
| `0x40000000`–`0x4FFFFFFF` | 256 M | **CHIF host shared-memory** window (the `phys−0x40000000` BAR, `docs/06`) |
| `0x80000000`–`0x807FFFFF` | 8 M | device |
| **`0x80F00000`–`0x80FFFFFF`** | 1 M | **interrupt controller IC1** (IRQ 32–63, `docs/12`) |
| `0x81000000`–`0x83FFFFFF` | 3×16 M | device |
| `0xA0000000`–`0xA000FFFF` | 64 K | device |
| **`0xA1000000`–`0xA1003FFF`** | 16 K | **GXELAN MAC DMA / secondary regs** (MAC driver uses `0xA10002xx`, `docs/11`) |
| **`0xB0000000`–`0xB3FFFFFF`** | 64 M | **KVM video-capture buffer region** (DVC framebuffers; controller reg `0x01F100C0`) — see docs/14 |
| **`0xC0000000`–`0xC001FFFF`** | 128 K | **ASIC core**: sys/crypto/**RTC**/**UART** (`0xC00000xx`), **GXELAN MAC0/MAC1** (`0xC0004000/5000`), MDIO, GXELAN reset `0xC00000C0`, 64-bit timer `0xC0000098` |
| `0xC0030000`–`0xC003FFFF` | 64 K | device |
| `0xCE000000`–`0xCE007FFF` | 32 K | device |
| **`0xCEFF0000`–`0xCEFF0FFF`** | 4 K | **interrupt controller IC0** (IRQ 0–31, `docs/12`) |
| `0xD0000000`–`0xD0FFFFFF` | 16 M | device |
| `0xD1000000`–`0xD100FFFF` | 64 K | device |
| `0xF0000000`–`0xFFEFFFFF` | 256 M | **lightly-used upper-address device aperture** (flags `0x1`). NOT flash/heap/peripheral. ~10 genuine word reads total; a top-of-space register block at `0xFF000000` (+0/4/8, chip ID/status?). The apparent heavy use was a false positive = crypto error codes `0xf16100xx`. Likely host-mem/PCI aperture or reserved map. |

## Notes

- This table is the direct source for OpenBMC DT `reg`/`ranges` — every base is physical.
- `0x40000000` confirms the CHIF code's `phys − 0x40000000` BAR math (`docs/06`).
- Still-unlabeled device windows (`0x80000000`, `0x81–83M`, `0xB0000000`, `0xD0000000`,
  `0xF0000000`) are candidates for KVM/video, host-memory access, and the SPI/boot-flash window —
  next RE targets.

- **`0x80200000`** = Gromit-XE **clock/PLL/reset controller** (boot ROM, docs/30): `+0x88` PLL
  ctrl, `+0x40`/`+0x58` reset/config. Strap-select register at `0x801F00A4`. Distinct from IC1
  (`0x80F00000`).

## Physical memory parts — CONFIRMED from the board (2026-10-21)

The fitted DL380p G7 iLO 3 memory devices, read off the chips:

| Role | Part | Notes |
|------|------|-------|
| **Boot ROM / SPI NOR** | **Macronix MX25L6455E** | 64 Mbit = **8 MB**, 3 V SPI NOR. Confirms the 8 MB flash (docs/01/03/15/17). JEDEC RDID = `C2 20 17`. Supports 4 KB sector (0x20) + 64 KB block (0xD8) erase → `spinor::{read_id, erase_4k, erase_64k}`. Mapped `0xFF800000-0xFFFFFFFF`; boot ROM/SPL at `0x7F0000`=`0xFFFF0000`. |
| **SRAM** | **ISSI IS62WV2568BLL-45** (async, 45 ns, low-power) | Small async SRAM at the low window (`0x20000000`). **This is the pre-DDR scratch RAM** — it resolves the docs/31 open question "how does the reset handler use stack `0x20004000` before DDR is trained": that address is **SRAM**, live at reset, no init needed. The SPL (`spl/src/main.rs`) scratch stack + the boot ROM both use it until DDR is up; after DDR init the controller presents DRAM at `0x20000000` (SRAM is the low-order alias / bootstrap tier). |
| **DRAM** | **ISSI IS43/46LD16640A** (×16) | The main DRAM. ISSI "**LD**" family = **LPDDR / mobile DDR**, which **refines docs/30's "DDR3" label** — confirm the exact generation + density from the ISSI datasheet. The boot-ROM init values (docs/30, fn `0x7f198c` / table `0x7f1a0c`) are RE'd from the firmware and remain authoritative for bring-up regardless of the generation name; total DRAM size is MEMCFG-strap-derived at runtime (docs/30), consistent with the ~60 MB usable figure (likely 2× these parts). |

Implication for the SPL: the `0x20004000` scratch stack is **real SRAM**, so the SPL's pre-DDR
phase is on solid ground (no reliance on "warm"/partially-trained DRAM). The DDR init in
`spl/src/ddr.rs` brings up the IS43/46LD16640A per the RE'd table.
