# iLO 3 "RTC" — there is no calendar-RTC register (software clock)

Date: 2026-10-20. Static RE of the type-2 kernel (`decompiled/kernel_type2.bin`,
`ARM:LE:32:v5`) + OS strings. **Corrects `docs/05`** (which labelled `0xC0000097–0xB0`
as an RTC date/time block). There is **no wall-clock RTC** in the Gromit-XE register
file. Time-of-day is a **software clock** in the INTEGRITY RTOS, seeded by the 64-bit
hardware counter and set over the network (SNTP).

## What the three "RTC" registers really are

Found by scanning the kernel for literals `0xC0000097`/`0xC0000098`/`0xC00000A0`:

| Reg | Real role | Evidence |
|-----|-----------|----------|
| `0xC0000097` | **timer latch / ack byte** — written `0` next to the timer tick-handler teardown (`0x1ad28`) and in the fw-update path (`0x342c4`) | `ldr =0xC0000097; strb #0,[r0]` |
| `0xC0000098` | **64-bit free-running counter** (the timebase) — read by the kernel "get time" helper `0x3468` with `ldrd r0,r1,[0xC0000000+0x98]` | already the timer clocksource (docs/12, `timer-gxe.c`) |
| `0xC00000A0` | **chip-ID / revision register** — read and range-checked against `~0x50000` constants (`0x50024`, `0x50100`), guarded by assert-logs; NOT date/time | `ldr[A0]; and #0xffff00; cmp #0x50100` (`0x1afb0`, `0x1d670`, `0x1d278`, …) |

`docs/05`'s "+0x98 read 8× = time field" was the kernel reading the **counter** (the
timebase), and the month-table formatter it cited lives in the OS and formats the
*software* clock — not a hardware calendar register.

## The actual time-of-day design

The INTEGRITY kernel exposes a POSIX-style **RealTimeClock** object (kernel strings:
`Timer @C RealTimeClock`, `NoPrimaryClock`, `TheTimeHasNeverBeenSet`,
`TheTimeMustNotBeNULL`, `AlarmIsNotSet`). It is a *software* `CLOCK_REALTIME`:

- The **primary clock** = the 64-bit hardware counter at `0xC0000098` (monotonic ticks).
- Wall-clock seconds = an **offset** added to the counter; the offset is set by the
  application via **SNTP/NTP** (OS strings: `SNTP`, `ntp`, `gettimeofday`) and/or the
  host/web UI.
- Before it is ever set, `CLOCK_REALTIME` reads as the epoch (1970) → this is exactly
  the known **"iLO 3 date off by 70 years"** changelog symptom. It is a software-clock
  default, not a dead RTC battery.

There is **no I2C RTC chip** and **no battery-backed calendar counter** in the register
map (searched: no `IODRTC`, no `DS13xx`/`M41Txx`/`PCF85xx` strings; the only "RTC" token
is the INTEGRITY *software* RealTimeClock object).

## Consequences for the OpenBMC port

- **Remove the `hpe,gxe-rtc` DT node** (done) — it mapped a latch byte + an ID register,
  not an RTC. No RTC driver is needed or possible for this block.
- **Timebase** = the `0xC0000098` clocksource (`timer-gxe.c`, already written).
- **Wall clock** = standard OpenBMC software time: `systemd-timesyncd` (SNTP) for sync +
  `fake-hwclock` to persist the last-known time to flash across reboots. This reproduces
  the original firmware's behaviour exactly.
- If cross-reboot wall-time without a network is required, seed from the **host RTC** over
  the host interface (CHIF/IPMI `Get SEL Time` equivalent) — the host ProLiant has the
  real PC RTC. No BMC-local RTC hardware to drive.
