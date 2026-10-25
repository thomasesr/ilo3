//! Fan control service — actuation + FAIL-SAFE (SAFETY-CRITICAL, docs/42).
//!
//! On G7 the System BIOS owns thermal *policy*; iLO is monitor + actuator +
//! fail-safe (docs/42). This service applies per-PWM targets derived from sensor
//! demand and — above all — guarantees the fail-safe: anything uncertain ⇒ fans
//! to MAX, never off. It is NOT a PID loop.
//!
//! Model (docs/42): per-sensor demand (temp vs caution/critical → 0-100%); each
//! PWM output = a grouping algorithm over its sensors (FASTEST = hottest wins =
//! the safe default; SINGLE/AVERAGE optional); clamp [Min,Max] + hysteresis;
//! LOCK overrides. Fail-safe: sensor read error / unconfigured ⇒ that PWM = Max;
//! boot/stall default = Max; critical temp ⇒ event + host graceful shutdown.
//!
//! Board data (sensor topology, thresholds, groupings, Min/Max) is SDR/runtime
//! (docs/25/42 Open) — the tables here are flagged PLACEHOLDERS. Do NOT trust on
//! hardware until validated under thermal load with real SDR data.

#![allow(dead_code)]

use core::fmt::Write;

use embassy_time::{Duration, Timer};

use crate::eventlog::{self, LogKind, Severity};
use crate::sensors::{self, Celsius};
use crate::uart::Uart;
use crate::{fan, power, rng};

/// Grouping algorithm for a PWM output (docs/42 GROUPINGS).
#[derive(Clone, Copy)]
pub enum Group {
    /// One sensor scaled by a weight percent.
    Single(u8),
    /// Hottest sensor in the zone wins (the safe default).
    Fastest,
    /// Mean demand of the zone.
    Average,
}

/// One PWM output: which fan channel, which sensors drive it, and its clamps.
#[derive(Clone, Copy)]
pub struct Pwm {
    pub channel: usize,          // fan.rs scan-chain channel
    pub sensors: &'static [usize], // indices into sensors::TABLE
    pub group: Group,
    pub min: u8, // duty floor (0..255) — keeps minimum airflow
    pub max: u8, // duty ceiling
    pub hyst: u8, // demand hysteresis (percent) to damp oscillation
    pub lock: Option<u8>, // manual override duty (CLI `p N lock X`)
}

/// PWM zone table — PLACEHOLDER (docs/42 Open: real groupings/clamps = board SDR).
pub const PWMS: &[Pwm] = &[
    Pwm { channel: 0, sensors: &[0, 2], group: Group::Fastest, min: 76, max: 255, hyst: 3, lock: None },
    Pwm { channel: 1, sensors: &[1, 2], group: Group::Fastest, min: 76, max: 255, hyst: 3, lock: None },
];

const TICK_MS: u64 = 500; // a few Hz is enough (docs/42)

/// Per-sensor demand: 0% at/below caution, ramping to 100% at/above critical.
/// Conservative — real per-sensor curve is board SDR (docs/42 Open).
fn demand(t: Celsius, thr: sensors::Thresholds) -> u8 {
    if t >= thr.critical {
        100
    } else if t <= thr.caution {
        0
    } else {
        let span = (thr.critical - thr.caution).max(1) as i32;
        ((t - thr.caution) as i32 * 100 / span) as u8
    }
}

#[inline]
fn pct_to_duty(pct: u8, min: u8, max: u8) -> u8 {
    let d = (pct as u32 * 255 / 100) as u32;
    d.clamp(min as u32, max as u32) as u8
}

/// Force EVERY fan to full. The boot/stall/fault default — call before the loop
/// so the hardware is never left cooling-off if the control task never runs.
pub fn init_failsafe() {
    for p in PWMS {
        fan::set_pwm(p.channel, 255);
    }
}

/// Evaluate one PWM's target duty. Any sensor read error in its group ⇒ Max
/// (fail-safe). `d[i]` is `None` when sensor `i` failed to read.
fn eval(p: &Pwm, d: &[Option<u8>]) -> u8 {
    if let Some(duty) = p.lock {
        return duty;
    }
    // Fail-safe: if any driving sensor is unreadable, run this fan full.
    if p.sensors.iter().any(|&i| d.get(i).copied().flatten().is_none()) {
        return p.max;
    }
    let dem = |i: usize| d[i].unwrap() as u32;
    let pct = match p.group {
        Group::Single(w) => (dem(p.sensors[0]) * w as u32 / 100) as u8,
        Group::Fastest => p.sensors.iter().map(|&i| dem(i)).max().unwrap_or(100) as u8,
        Group::Average => {
            let n = p.sensors.len().max(1) as u32;
            (p.sensors.iter().map(|&i| dem(i)).sum::<u32>() / n) as u8
        }
    };
    pct_to_duty(pct, p.min, p.max)
}

/// The fan control task: read sensors, drive each PWM, enforce the fail-safe and
/// the critical-temp shutdown. A missed/failed read defaults the zone to Max.
#[embassy_executor::task]
pub async fn thermal_task() {
    let mut con = Uart::console();
    let mut last = [0u8; 16]; // last applied duty per PWM (index = PWMS order)
    let mut crit_handled = false;
    let mut tick = 0u32; // rotates the stirred tach channel

    // Boot default: full cooling until the first good sample proves otherwise.
    init_failsafe();
    for (i, _) in PWMS.iter().enumerate() {
        last[i] = 255;
    }
    let _ = writeln!(con, "[thermal] fail-safe init: all fans MAX");

    loop {
        // Read every sensor once; None = read error (→ fail-safe in eval).
        let mut d = [None; 16];
        let mut critical = false;
        for (i, s) in sensors::TABLE.iter().enumerate() {
            match s.read() {
                Ok(t) => {
                    d[i] = Some(demand(t, s.thr));
                    if t >= s.thr.critical {
                        critical = true;
                    }
                }
                Err(_) => {
                    let _ = writeln!(con, "[thermal] sensor {} READ FAIL -> zone MAX", s.name);
                    eventlog::raise(
                        LogKind::Iml,
                        Severity::Caution,
                        eventlog::class::THERMAL,
                        eventlog::code::SENSOR_FAIL,
                        "temperature sensor read failure — fans forced to maximum",
                        None,
                    );
                }
            }
        }

        // Critical temperature: log + graceful host shutdown (once), keep fans max.
        if critical && !crit_handled {
            let _ = writeln!(con, "[thermal] CRITICAL temp -> host graceful shutdown");
            eventlog::raise(
                LogKind::Iml,
                Severity::Critical,
                eventlog::class::THERMAL,
                eventlog::code::CRITICAL_TEMP,
                "critical temperature threshold exceeded — automatic OS shutdown",
                None,
            );
            init_failsafe();
            power::graceful_shutdown();
            crit_handled = true;
        }

        // Apply each PWM, with demand hysteresis to damp oscillation.
        for (i, p) in PWMS.iter().enumerate() {
            let target = eval(p, &d);
            let prev = last[i];
            let hyst_duty = (p.hyst as u32 * 255 / 100) as u8;
            if target >= prev || prev - target > hyst_duty || target == p.max {
                fan::set_pwm(p.channel, target);
                last[i] = target;
            }
        }

        // Stir the entropy pool (docs/32): fold in one fan's raw tach count,
        // the per-sensor demands (temperature noise), and the counter value at
        // this instant (read-timing jitter). Supplementary, low-credit noise —
        // never the primary seed; the sponge pool conditions it.
        let ch = (tick as usize) % fan::NR_CHAN;
        let tach = fan::read_tach_raw(ch) as u32;
        let temp_mix = d.iter().flatten().fold(0u32, |a, &x| (a << 3) ^ x as u32);
        rng::feed_timing(crate::timer::now_ticks() as u32 ^ (tach << 8) ^ temp_mix);
        tick = tick.wrapping_add(1);

        Timer::after(Duration::from_millis(TICK_MS)).await;
    }
}
