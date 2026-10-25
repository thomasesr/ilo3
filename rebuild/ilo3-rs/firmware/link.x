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

/* Reserved top 1 MB of RAM = the high-vector alias. With the MMU on (V=1) the
 * CPU exception base is 0xFFFF0000; src/mmu.rs maps section VA 0xFFF00000 to
 * this dedicated physical MB so writable RAM backs the vector table (vectors
 * then live at _vectors_phys + 0xF0000). Bring-up shortcut: a whole 1 MB section
 * for 64 B of vectors — an L2 coarse page table would trim it later. */
_vectors_phys = ORIGIN(RAM) + LENGTH(RAM) - 0x100000;   /* 0x20F00000, 1 MB aligned */

/* Top-of-usable-RAM downward-growing stacks, just below the vector alias. 64 KB
 * reserved for bring-up: IRQ mode gets the top 4 KB, SVC mode the rest. Both set
 * in _start (src/main.rs). */
_stack_top = _vectors_phys;
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

  /* Heap: the DDR gap between .bss and the stack (src/heap.rs feeds it to the
   * global allocator). NOT zeroed by _start (lies outside _sbss.._ebss), so it
   * costs nothing at boot; the allocator owns it. Several MB on the 16 MB map. */
  . = ALIGN(8);
  _heap_start = .;
  _heap_end = _stack_top - _stack_size;   /* up to the stack bottom */

  /* Reserve the stack so it never overlaps .bss/heap. */
  .stack (NOLOAD) : ALIGN(8)
  {
    . = _stack_top - _stack_size;
    . = _stack_top;
  } > RAM

  /* Reserve the top 1 MB — the MMU high-vector alias target (src/mmu.rs maps
   * VA 0xFFF00000 here). Dedicated: nothing else may be placed in this MB. */
  .vectoralias (NOLOAD) : ALIGN(0x100000)
  {
    . = _vectors_phys;
    . = _vectors_phys + 0x100000;
  } > RAM

  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment) }
}
