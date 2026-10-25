/* iLO 3 (Gromit-XE, ARM926EJ-S) SPL linker script — runs XIP from flash.
 *
 * The ARM926 resets into the vector base; this board straps VINITHI=1, so reset
 * fetches from 0xFFFF0000 (high vectors) = SPI flash offset 0x7f0000 (the 8 MB
 * flash is mapped 0xFF800000-0xFFFFFFFF; docs/03, docs/31). This SPL replaces
 * HP's signed boot ROM: it runs in place from flash, brings up clock + DDR3
 * (docs/30), copies the main firmware from flash into DDR, and jumps to it.
 *
 * XIP constraint: DRAM does not exist yet, so the SPL must live entirely in
 * flash (.text + .rodata) and touch NO writable statics before DDR is up. There
 * is no .data/.bss load here — a scratch stack (0x20004000, the address HP's
 * boot ROM itself uses pre-DDR) is set in _start; after DDR init the stack is
 * raised into high DRAM before the image copy (src/main.rs).
 */
ENTRY(_start);

MEMORY
{
  FLASH (rx) : ORIGIN = 0xFFFF0000, LENGTH = 64K   /* flash 0x7f0000..0x800000 */
}

/* Pre-DDR scratch stack (HP boot ROM uses this exact address at reset) and the
 * post-DDR high stack, raised before the image copy so it can't clobber it. */
_spl_scratch_sp = 0x20004000;
_spl_high_sp    = 0x20F00000;

SECTIONS
{
  . = ORIGIN(FLASH);

  .text : ALIGN(4)
  {
    KEEP(*(.text.vectors));   /* reset vector table FIRST, at 0xFFFF0000 */
    *(.text .text.*);
  } > FLASH

  .rodata : ALIGN(4) { *(.rodata .rodata.*); } > FLASH

  .data : { *(.data .data.*) } > FLASH
  .bss  : { *(.bss .bss.* COMMON) } > FLASH

  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment) }
}

/* The SPL must have no initialised or zero-init statics (no RAM before DDR).
 * Fail the build if any slip in, rather than silently relying on dead DRAM. */
ASSERT(SIZEOF(.data) == 0, "SPL has a .data section: no writable statics allowed pre-DDR");
ASSERT(SIZEOF(.bss)  == 0, "SPL has a .bss section: no writable statics allowed pre-DDR");
