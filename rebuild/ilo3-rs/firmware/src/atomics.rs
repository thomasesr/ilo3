//! Atomic libcall shims for single-core ARM926EJ-S (ARMv5TE, no LDREX/STREX).
//!
//! Our custom target (`armv5te-ilo3.json`) claims `atomic-cas` + 32-bit atomics
//! so that crates using `core::sync::atomic` (embassy-executor, cordyceps) build
//! here. ARMv5 has no load-linked/store-conditional, so LLVM lowers those atomic
//! ops to `__atomic_*` libcalls — which we provide below as critical sections:
//! on a single core, masking IRQs (see `critsec.rs`) makes any read-modify-write
//! atomic w.r.t. every other context. The memory-order argument is irrelevant
//! once interrupts are off, so it is ignored.
//!
//! This is the ARMv5 bring-up friction flagged in PLAN-rust.md. Correct only
//! because the BMC is single-core; an SMP target would need real LL/SC.

#![allow(dead_code)] // siblings kept for the next crates (embassy-time) to link against

use core::ptr::{read_volatile, write_volatile};

/// Run `f` with IRQs masked (single-core atomicity).
#[inline(always)]
fn atomic<R>(f: impl FnOnce() -> R) -> R {
    critical_section::with(|_| f())
}

// ---- load / store ----

#[no_mangle]
pub unsafe extern "C" fn __atomic_load_4(ptr: *const u32, _order: i32) -> u32 {
    atomic(|| read_volatile(ptr))
}

#[no_mangle]
pub unsafe extern "C" fn __atomic_store_4(ptr: *mut u32, val: u32, _order: i32) {
    atomic(|| write_volatile(ptr, val))
}

#[no_mangle]
pub unsafe extern "C" fn __atomic_load_1(ptr: *const u8, _order: i32) -> u8 {
    atomic(|| read_volatile(ptr))
}

#[no_mangle]
pub unsafe extern "C" fn __atomic_store_1(ptr: *mut u8, val: u8, _order: i32) {
    atomic(|| write_volatile(ptr, val))
}

// ---- exchange / compare-exchange ----

#[no_mangle]
pub unsafe extern "C" fn __atomic_exchange_4(ptr: *mut u32, val: u32, _order: i32) -> u32 {
    atomic(|| {
        let old = read_volatile(ptr);
        write_volatile(ptr, val);
        old
    })
}

/// Returns true and stores `desired` iff `*ptr == *expected`; otherwise writes
/// the current value back through `expected` and returns false (C11 semantics).
#[no_mangle]
pub unsafe extern "C" fn __atomic_compare_exchange_4(
    ptr: *mut u32,
    expected: *mut u32,
    desired: u32,
    _success: i32,
    _failure: i32,
) -> bool {
    atomic(|| {
        let cur = read_volatile(ptr);
        if cur == read_volatile(expected) {
            write_volatile(ptr, desired);
            true
        } else {
            write_volatile(expected, cur);
            false
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn __atomic_compare_exchange_1(
    ptr: *mut u8,
    expected: *mut u8,
    desired: u8,
    _success: i32,
    _failure: i32,
) -> bool {
    atomic(|| {
        let cur = read_volatile(ptr);
        if cur == read_volatile(expected) {
            write_volatile(ptr, desired);
            true
        } else {
            write_volatile(expected, cur);
            false
        }
    })
}

// ---- fetch-and-op (return the previous value) ----

macro_rules! fetch_op_4 {
    ($name:ident, $op:expr) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name(ptr: *mut u32, val: u32, _order: i32) -> u32 {
            atomic(|| {
                let old = read_volatile(ptr);
                write_volatile(ptr, $op(old, val));
                old
            })
        }
    };
}

fetch_op_4!(__atomic_fetch_and_4, |a: u32, b: u32| a & b);
fetch_op_4!(__atomic_fetch_or_4, |a: u32, b: u32| a | b);
fetch_op_4!(__atomic_fetch_xor_4, |a: u32, b: u32| a ^ b);
fetch_op_4!(__atomic_fetch_add_4, |a: u32, b: u32| a.wrapping_add(b));
fetch_op_4!(__atomic_fetch_sub_4, |a: u32, b: u32| a.wrapping_sub(b));
