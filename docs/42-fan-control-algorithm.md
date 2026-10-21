# iLO 3 fan control algorithm — RE (SAFETY-CRITICAL)

Date: 2026-10-21. Static RE of `os_main_base0.bin` (fan controller `~0x118000-0x128000`).
Maps the thermal→fan algorithm. **This is safety-critical**: a wrong reimplementation overheats
and damages the hardware. The rewrite MUST reproduce the fail-safe behaviour exactly.

## CORRECTION (ownership): the System ROM/BIOS owns thermal policy, not iLO

On ProLiant **G7**, the **System ROM (BIOS) runs the fan/thermal control policy** — iLO is
**monitor + actuator + fail-safe**, not the thermal brain. Evidence: the BIOS↔iLO mailbox
(`ROMCHIF_MSGID_*`) carries only **power** messages (BURN/cap PID, CPU F-stop, topology) — **no
fan/thermal command**. The per-sensor demand curve + thresholds + sensor→PWM groupings are
**platform/SDR data the BIOS provisions**, not iLO logic. So the grouping algorithm below is iLO's
**actuation/application layer** (apply the targets the platform sets) + its hardware-level
**fail-safe**, not an autonomous control loop.

**For the Rust rewrite, iLO's fan responsibility is small**: (1) read temps/fan-status/tach for
health/SNMP/IML; (2) drive the PWM scan-chain as directed; (3) the **fail-safe** (force MAX on
sensor fault / controller stall / critical). The normal thermal *policy* is the BIOS's — the
rewrite does NOT reimplement the thermal brain. This also removes most of the "safety-critical
algorithm" burden (the fail-safe net remains iLO's and is simple: default/fault ⇒ MAX).

## Model: sensor→PWM grouping algorithms (iLO actuation layer)

Fan control is **not** a PID loop. (The PID strings in the image — `ROMCHIF_MSGID_BURN_SET_PID_*` —
are the separate **power-cap** controller, `docs/43`.) The fan model:

1. **Per-sensor demand**: each thermal sensor yields a fan **demand** (an output %), derived from
   its temperature vs its SDR thresholds (caution/critical). Proportional/curve response per sensor.
2. **Grouping algorithms** (`GROUPINGS`): each **PWM output** is driven by an algorithm over a set
   of sensors:
   - **`SINGLE`** — one sensor × a **weighting** → output.
   - **`FASTEST`** — the **max** demand of the sensor group (the hottest wins). *The safe default.*
   - **`AVERAGE`** — the mean demand of the group.
   Dump format: `%d: SINGLE Output:%3d (Sensor:%d - weighting:..)`, `%d: FASTEST Output:%3d [..]`.
   "NO algorithms enabled" if none configured.
3. **Per-PWM clamp + modifiers** (table `No. PWM Min Max Hyst BLOW LOCK PCT Sta`):
   - **Min / Max** — output floor/ceiling.
   - **Hyst** — hysteresis (anti-oscillation).
   - **PCT** — ramp/percentage step.
   - **LOCK** — manual override (fixed speed; CLI `p N lock X`).
   - **BLOW** — blowout (force max for a period; purge / on caution).
   - **Status**.

Per-PWM record fields (from the struct accesses): config bytes at `+0x11..+0x18`
(Min/Max/Hyst/Blow/Lock/PCT), current/output words at `+0x10`/`+0x14`, status/flags `+0x1c`,
list ptr `+0x0c`. The sensor→PWM **groupings + weightings are SDR/board data** (runtime, not in
the binary) — same as the sensor topology (`docs/25`).

## Fail-safe behaviour — MUST replicate exactly

| Condition | Action |
|-----------|--------|
| **Sensor read failure** (`Sensor update FAIL` / `SENSOR fail`) | **Force the affected PWM(s) to MAX** — can't trust temp ⇒ run fans full |
| **No algorithm / unconfigured** | fans default high (fail to cooling-on, not cooling-off) |
| **Caution threshold** | raise fans (BLOWOUT may engage) |
| **Critical temp** (`Critical Temperature Threshold Exceeded`) | log IML event + **`Automatic Operating System Shutdown`** (graceful host shutdown), then power-off to protect hardware |
| **Startup / purge** | `BLOWOUT` = `ALL_FANS` to full for a period |
| **Controller fault / boot** | default MAX |

Overrides (CLI `p N on|off|min|max|hyst|blow|pctramp|zero|lock X|feton|go|status`): `Force to
max` / `Force to min` / `Force to ZERO`, per-PWM lock.

## Rust rewrite mapping — the safe design

- A per-sensor demand function (temp vs caution/critical → 0-100%).
- Per-PWM group evaluator: **default to `FASTEST`** (max of the zone's sensors) — the conservative
  choice. Support SINGLE/AVERAGE if the board SDR defines them.
- Clamp [Min, Max] + hysteresis + ramp; honour LOCK.
- **Fail-safe is the #1 invariant**:
  - **Any** sensor read error / stale sample / unconfigured zone → that PWM = **Max**.
  - **Watchdog**: if the control task stalls, hardware/boot default must be **Max** (fans full),
    never off. Set the scan-chain to max at init before the loop starts.
  - **Critical temp** → emit the event + trigger host shutdown (via power control, `docs/24`).
- Tick rate: a few Hz is enough; never skip a tick silently (missed ticks → assume fault → max).
- Validate against hardware with a thermal load before trusting it (the demand curve + thresholds
  are board-specific).

## Open (needed before trusting on hardware)

- The exact **per-sensor demand curve** (temp→% mapping) and the SDR **threshold values** (caution/
  critical per sensor) — board/SDR data; read from a live iLO SDR dump or a datasheet.
- The sensor→PWM **grouping table + weightings** for the specific board (DL380p Gen7 etc.).
- The **BLOWOUT** duration + trigger thresholds; the ramp (PCT) step semantics.
- **Hardware validation**: run the rewritten controller under thermal load and confirm fans track
  temp + the fail-safe forces max. Do NOT ship fan control unvalidated.
