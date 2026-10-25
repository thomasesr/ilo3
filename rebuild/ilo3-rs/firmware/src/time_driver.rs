//! `embassy-time` Driver over the Gromit-XE monotonic counter + Wallace tick.
//!
//! Implements the global `embassy_time_driver::Driver` so `embassy_time::Timer`,
//! `Instant` and `Duration` work — replacing the Phase-1 busy-wait `delay_us`.
//!
//!   * `now()` = the free-running 64-bit counter (`timer::now_ticks`), already at
//!     `TIMER_HZ` = 1 MHz, matching embassy-time's `tick-hz-1_000_000` feature,
//!     so one embassy tick == one hardware counter tick (no scaling).
//!   * Wakeups are **tick-based**: `schedule_wake` parks the waker in a small
//!     heapless queue; the periodic Wallace clockevent IRQ (36) calls `on_tick`,
//!     which wakes every deadline that has elapsed. Timer resolution therefore
//!     equals the Wallace period (coarse; the HW rate is still CONFIRM-ON-HW).
//!
//! A finer one-shot design (arm the Wallace reload for the exact next deadline)
//! can replace the periodic model once the Wallace reload/ack semantics are
//! validated on hardware — `schedule_wake` already has the next-deadline hook.

use core::cell::RefCell;
use core::task::Waker;

use critical_section::Mutex;
use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;

use crate::timer;

struct GxeTimeDriver {
    queue: Mutex<RefCell<Queue>>,
}

embassy_time_driver::time_driver_impl!(
    static DRIVER: GxeTimeDriver = GxeTimeDriver {
        queue: Mutex::new(RefCell::new(Queue::new())),
    }
);

impl Driver for GxeTimeDriver {
    #[inline]
    fn now(&self) -> u64 {
        timer::now_ticks()
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        critical_section::with(|cs| {
            // Park the waker. Expiry is serviced by the periodic Wallace tick
            // (`on_tick`), so there is no one-shot alarm to arm here.
            self.queue.borrow(cs).borrow_mut().schedule_wake(at, waker);
        });
    }
}

/// Service expired timers: wake every queued waker whose deadline has passed.
/// Call from the Wallace tick IRQ handler (runs in IRQ mode, IRQs masked).
pub fn on_tick() {
    critical_section::with(|cs| {
        let now = timer::now_ticks();
        // `next_expiration` wakes all items with `deadline <= now`; the returned
        // next deadline is unused in tick mode — we re-check on every tick.
        let _ = DRIVER.queue.borrow(cs).borrow_mut().next_expiration(now);
    });
}
