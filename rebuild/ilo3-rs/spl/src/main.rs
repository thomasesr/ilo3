//! iLO 3 (Gromit-XE, ARM926EJ-S) first-stage loader (SPL).
//!
//! Replaces HP's signed boot ROM (docs/31 "replace the boot ROM with an unsigned
//! first-stage"): runs XIP from flash at 0xFFFF0000 (high vectors, reset target),
//! brings up DDR3 (docs/30), copies the main firmware from flash into DDR, and
//! jumps to it. Clean-room: register addresses/values are RE'd facts, no HP code.
//!
//! Pre-DDR discipline (see link-spl.x): no .data/.bss, no statics, no heap — only
//! .text/.rodata in flash + a scratch stack at 0x20004000 (the address HP's boot
//! ROM uses at reset — this is ISSI IS62WV2568BLL async SRAM, live at reset, no
//! init needed; docs/13). After DDR is up the stack is raised into high DRAM
//! before the image copy, which targets the low DRAM the scratch stack sat near.
#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

mod ddr;
mod flash;
mod uart;

/// Post-DDR stack top (high DRAM), raised before the image copy so it cannot
/// clobber the firmware being written at 0x20000000. Matches the main image's
/// reserved top-of-RAM (firmware/link.x `_vectors_phys`).
const HIGH_SP: u32 = 0x20F0_0000;

// ---- reset vector table + cold-reset handler (XIP at 0xFFFF0000) ----
global_asm!(
    ".section .text.vectors",
    ".global _start",
    "_start:",
    "    b _reset",              // reset   (fetched at 0xFFFF0000 on power-on)
    "    b _hang_v",             // undef
    "    b _hang_v",             // swi
    "    b _hang_v",             // prefetch abort
    "    b _hang_v",             // data abort
    "    b _hang_v",             // reserved
    "    b _hang_v",             // irq
    "    b _hang_v",             // fiq
    "_hang_v:",
    "    b _hang_v",             // no exception handling in the SPL
    "_reset:",
    "    msr  cpsr_c, #0xD3",    // SVC mode, IRQ+FIQ masked
    // MMU + I/D caches off (flat physical XIP), invalidate caches + TLB
    "    mrc  p15, 0, r0, c1, c0, 0",
    "    bic  r0, r0, #0x1",     // M = 0 (MMU off)
    "    bic  r0, r0, #0x4",     // C = 0 (D-cache off)
    "    bic  r0, r0, #0x1000",  // I = 0 (I-cache off)
    "    mcr  p15, 0, r0, c1, c0, 0",
    "    mov  r0, #0",
    "    mcr  p15, 0, r0, c7, c7, 0",  // invalidate I+D caches
    "    mcr  p15, 0, r0, c8, c7, 0",  // invalidate TLB
    "    ldr  sp, =_spl_scratch_sp",   // pre-DDR scratch stack (0x20004000)
    "    bl   spl_main",
    // spl_main is -> !, but park safely if it ever returns
    "1:  mcr  p15, 0, r0, c7, c0, 4",  // CP15 wait-for-interrupt
    "    b    1b",
);

#[no_mangle]
pub extern "C" fn spl_main() -> ! {
    uart::puts("\n[spl] iLO3 Gromit-XE first-stage loader\n");

    // Clock self-check: the operative boot path (ROM fn 0x7f198c, docs/30) trains
    // DDR on the strap-default clock and does NOT reprogram the PLL. That holds
    // only when the clock-select strap is non-zero; mode 0 would need the
    // strap->PLL decode path this SPL does not implement — warn loudly if seen.
    let mode = ddr::clock_mode();
    uart::puts("[spl] clock mode ");
    uart::puthex(mode);
    if mode == 0 {
        uart::puts(" — WARNING: PLL-decode path not implemented, clock may be wrong\n");
    } else {
        uart::puts(" (strap-default clock, no PLL reprogram)\n");
    }

    match ddr::init() {
        ddr::Status::Warm => uart::puts("[spl] ddr: warm boot — DRAM already up\n"),
        ddr::Status::Cold => uart::puts("[spl] ddr: cold init complete\n"),
        ddr::Status::CalTimeout => {
            uart::puts("[spl] ddr: PHY calibration TIMEOUT — DRAM unusable, halt\n");
            flash::hang();
        }
    }

    // DRAM is up: raise the stack into high DRAM, then hand off to the image
    // copy + jump (never returns). Done in asm so SP is switched atomically
    // before any further stack use — the new frame comes from HIGH_SP.
    // SAFETY: DDR is initialised, so HIGH_SP is valid RAM; copy_and_jump diverges.
    unsafe {
        core::arch::asm!(
            "mov sp, {s}",
            "b   {f}",
            s = in(reg) HIGH_SP,
            f = sym crate::flash::copy_and_jump,
            options(noreturn),
        );
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    uart::puts("\n[spl] PANIC\n");
    flash::hang();
}
