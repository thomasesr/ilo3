//! Global allocator over the linker-reserved DDR heap (Phase 4).
//!
//! A linked-list first-fit heap (embedded-alloc, locked via our critical-section
//! impl) over the DDR gap between .bss and the stack (link.x `_heap_start` ..
//! `_heap_end`). Enables `alloc` for the TLS session state machine and the
//! alloc-using management services. Several MB on the 16 MB bring-up map.

use core::ptr::addr_of;

use embedded_alloc::LlffHeap as Heap;

#[global_allocator]
static HEAP: Heap = Heap::empty();

extern "C" {
    static _heap_start: u8;
    static _heap_end: u8;
}

/// Initialise the global allocator. Call once at boot, after the MMU is up (the
/// heap lives in cacheable DDR) and before any allocation. Returns the heap size.
pub fn init() -> usize {
    let start = addr_of!(_heap_start) as usize;
    let end = addr_of!(_heap_end) as usize;
    let size = end - start;
    // SAFETY: the region is linker-reserved and exclusively the heap's; valid
    // DDR after bring-up; called exactly once.
    unsafe { HEAP.init(start, size) };
    size
}
