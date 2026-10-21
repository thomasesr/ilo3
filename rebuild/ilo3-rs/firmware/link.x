/* iLO 3 (Gromit-XE, ARM926EJ-S) linker script — Phase 1 bring-up.
 *
 * Physical memory map (docs/13, docs/30):
 *   DDR RAM   0x20000000+   (strap-sized; boot ROM used stack 0x20004000)
 *   SPI flash 0xFF800000    (8 MB, XIP, image store — SPL copy is a later phase)
 *   peripherals 0xC0000000 / 0xCEFF0000 / 0x80F00000 / 0x01F0xxxx (MMIO, not here)
 *
 * Phase 1 model: a single flat image linked to RUN FROM DDR (VMA = LMA =
 * 0x20000000), dropped there by an SPL/loader/debugger. Flash-XIP + copy-to-RAM
 * comes with the SPL sub-phase. 16 MB reserved — safe lower bound on DDR size.
 */
ENTRY(_start);

MEMORY
{
  RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 16M
}

/* Top-of-RAM downward-growing stacks. 64 KB reserved for bring-up: the IRQ
 * mode gets the top 4 KB, SVC mode the rest. Both set in _start (src/main.rs). */
_stack_top = ORIGIN(RAM) + LENGTH(RAM);
_stack_size = 0x10000;
_irq_stack_top = _stack_top;              /* IRQ-mode banked sp */
_svc_stack_top = _stack_top - 0x1000;     /* SVC-mode sp (below the IRQ stack) */

SECTIONS
{
  . = ORIGIN(RAM);

  .text : ALIGN(4)
  {
    KEEP(*(.text._start));   /* reset entry first, at 0x20000000 */
    KEEP(*(.text.vectors));  /* exception vector table + IRQ entry (vectors.rs) */
    *(.text .text.*);
  } > RAM

  .rodata : ALIGN(4) { *(.rodata .rodata.*); } > RAM

  .data : ALIGN(4)
  {
    _sdata = .;
    *(.data .data.*);
    . = ALIGN(4);
    _edata = .;
  } > RAM

  .bss (NOLOAD) : ALIGN(4)
  {
    _sbss = .;
    *(.bss .bss.*);
    *(COMMON);
    . = ALIGN(4);
    _ebss = .;
  } > RAM

  /* Reserve the stack so it never overlaps .bss/heap. */
  .stack (NOLOAD) : ALIGN(8)
  {
    . = _stack_top - _stack_size;
    . = _stack_top;
  } > RAM

  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment) }
}
