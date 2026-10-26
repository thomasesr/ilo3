# iLO self-reset + UID-button-hold reset (RE + implementation)

How HP's firmware reboots the iLO ASIC (e.g. after a firmware update), what is and
isn't pin-able statically, and the clean-room UID-button-hold reset we built on top.

## 1. How HP resets the iLO after a firmware update (RE, `ilo3_194.bin`)

- The self-reset entry is **`reset_ilo()`**. The firmware-update path calls it
  directly: string `http_flash_complete_thread: calling reset_ilo()...` (@`0x479384`),
  logged at the caller `0x479174` right before the reset. So: flash the image →
  `reset_ilo()` → reboot into the new firmware.
- A **reset-reason table** feeds the event log: `iLO reset for Firmware upgrade.`
  (`0x91d64`), `iLO reset by watchdog.`, `by host`, `by user diagnostics.`,
  `for configuration changes.`, `to factory defaults.`
- **The actual HW reset is NOT a single app-level register.** `reset_ilo` logs the
  reason and routes the reset through **INTEGRITY IODevice kernel objects**; the
  write to whatever reset/clock line the ASIC uses lives behind that abstraction,
  not as a literal in the application image.
- **`bmc_wdt_reset` / `bmc_wdt_*` is the IPMI HOST watchdog**, not the iLO self-reset
  (string `BMC IPMI Watchdog Timer Timeout`). It calls driver methods `0x210eebc`
  (`stop_timer`) / `0x210f880` on a WDT object at RAM global `0x62600` — a dead end
  for "reset the BMC".
- Boot-ROM `0x80200000+0x58` (docs/30) is clock/geometry config (`0xFFFF3c2c`), NOT
  a reset trigger.

**Conclusion:** there is no clean "watchdog/reset register" to mirror. HP's reset is
kernel-mediated. See §3 for the primitive we use instead.

## 2. Front-panel buttons on the CPLD (RE)

The system CPLD (`0x01F14000`, docs/08/09) dispatches **6 interrupt sources** over
input registers **`+0x08 / +0x09 / +0x0D` and `+0x2C / +0x2D`**. From the dispatcher
(`0x2d2380`+, jump-table at `0x2d23a4`, CPLD reads via `iod_cpld` `0x1a3ee94`):

| source | input reg | bit tested | identity |
|--------|-----------|-----------|----------|
| 2 | `0x01F14008` | bit6 (`0x40`) | **POWER button (CONFIRMED)** |
| 0/1 | `0x01F14009` / `0x01F1402C` | bit2 | input (UID candidate) |
| 5 | `0x01F1400D` / `0x01F1402D` | bit2 | input (UID candidate) |
| 3 | (globals) | — | input |

The UID button is one of the non-power sources, but the handlers dispatch by index
and chain through helpers with no literal "UID" marker at the bit test, so the exact
UID register/bit **cannot be cleanly disambiguated statically**.

### Pin the UID bit LIVE (seconds, once the host debug channel is up)
With the BAR-size fix in place (docs/28 §10) and `dbgchif` reachable:
```
md 0x01F14008   md 0x01F14009   md 0x01F1400D   md 0x01F1402D
```
Press + release the UID button; the bit that flips is it. Set the three consts in
`firmware/src/cpld.rs` (`R_INPUT_UID`, `UID_BTN_BIT`, `UID_BTN_ACTIVE_HIGH`) and the
matching pair in `spl/src/recovery.rs::uid_held`.

## 3. Implementation (clean-room)

**Reset primitive — `firmware/src/reset.rs::soft_reset()`** (shared by `dbgchif`
reboot + the button monitor). Masks IRQ/FIQ, MMU + caches off, invalidate, `bx
0xFFFF0000` → re-runs the SPL boot chain. It is a **soft** reset: it deliberately
does NOT reset the PCI endpoint, so the host keeps the iLO enumerated across the
reset — exactly right for button → SPL-recovery with the host still up.

**UID-button-hold monitor — `firmware/src/uidreset.rs`** (main task, spawned in
`main.rs`). Polls `cpld::uid_button_pressed()` every 100 ms; a continuous **10 s**
hold → `soft_reset()`. *Safety:* it ARMS only after first seeing the button
RELEASED, so a mis-identified, permanently-asserted bit never triggers a boot-reset
loop (it just never arms). Placement is `main`, not the SPL: the SPL runs once at
boot and exits, so it cannot catch a hold during normal operation.

**SPL boot-hold → force recovery — `spl/src/recovery.rs`**, feature
**`uid-boot-recovery` (DEFAULT-OFF)**. Holding UID at boot makes the recovery window
wait indefinitely for the host (vs the bounded budget). Gated off because a
mis-identified stuck bit would hang every boot in recovery; enable only after §2
pins the bit. The always-safe path is the main-task monitor above.

### Why this matters (recovery)
On a ProLiant, iLO runs on standby and resets independently of host power. So
**hold UID 10 s → iLO soft-resets → SPL recovery window (minutes, docs/28) opens →
dom0 is still up and drives recovery** — an iLO-only reset with no AC cycle. It
covers the common bring-up failure (main runs, host channels broken). A totally
dead main can't run the monitor → only AC cycle helps there.
