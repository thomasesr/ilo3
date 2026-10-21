# iLO 3 Power Regulator + power capping — RE for the clean-room rewrite

Date: 2026-10-21. Static RE of `os_main_base0.bin`. Maps what iLO **actually controls** for
power/performance (vs fan/thermal, which is the System ROM's — `docs/42`). Two things: the
**Power Regulator** (P-state policy) and **dynamic power capping** (watt cap via CPU throttling),
both via the BIOS over ROM-CHIF.

## Power Regulator — P-state power mode (iLO-owned)

iLO sets the host CPU **power-management policy** (`POWER_REGULATOR_MODE`, `power_regulator_mode`,
`power_saver`; capabilities `POWER_REGULATOR_CAPS` / `power_regulator_capabilities_get`; `pstate`/
`p6State`). Four modes:

| Mode | Behaviour |
|------|-----------|
| **OS Control** | the host OS/ACPI manages P-states (iLO doesn't force) |
| **Dynamic Power Savings** | iLO/platform dynamically scales P-states to load (HP default) |
| **Static Low Power** | force the CPUs to a **low** P-state (min power) |
| **Static High Performance** | force the CPUs to the **highest** P-state (max perf) |

Set/read via web (`json/power_regulator`, `docs/33`), CLI, RIBCL (`SET_HOST_POWER_SAVER` /
`GET_HOST_POWER_SAVER`, `docs/37`), IPMI OEM (`OEMHP_POWER`, `docs/36`). The mode is pushed to the
BIOS/CPU (the platform applies the P-state limits); iLO stores it + exposes it.

## Dynamic power capping — the "BURN" PID (iLO ↔ BIOS over ROM-CHIF)

iLO enforces a **power cap** (max watts) by driving CPU frequency down. This **is** the PID in the
image (`ROMCHIF_MSGID_BURN_SET_PID_{GAIN,KP,KD}`) — a power-control PID, **not** fan control:

- **Measurement / range**: `BURN_TEST_START` / `IDLE_TEST_START` / `MAX_BURN` / `MIN_BURN` /
  `PWR_BURN_VER` — a "power burn" calibration measures the system's min/max power envelope.
- **PID**: iLO sets `SET_PID_GAIN`/`KP`/`KD`; the loop drives measured power toward the cap.
- **Actuation**: `CPUFSTOP_NOTIFY` — CPU **frequency-stop / throttle** states applied to hold the
  cap (processor clock-modulation). `TOPOLOGY_NOTIFY` = CPU topology for the controller.
- **Cap config**: `power_cap` / `power_capabilities` endpoints (`docs/33`), RIBCL power-cap commands,
  DCMI `dcmi_set_power_limit` (`docs/36`). `MIN_CAP_X` = minimum achievable cap.
- Source data = the PSU/power telemetry (`docs/20`) + host power measurement.

So: **iLO = the power-cap controller** (PID over measured watts), **the BIOS/power-micro = the
actuator** (applies CPU F-stop / P-state limits). Transport = ROM-CHIF (`src\romchfservice_c.c`,
the host-ROM channel, `docs/22`).

## Rust rewrite mapping

- **Power Regulator**: a simple mode setting (enum: OsControl / DynamicSavings / StaticLow /
  StaticHigh) persisted in NVRAM (`docs/38`), exposed on all surfaces (`bmc-service-wiring`), and
  pushed to the host/BIOS over the CHIF/ROM-CHIF link (`docs/28`) — the platform applies it.
- **Power capping** (optional, if wanted): a PID over measured power (PSU telemetry `docs/20` +
  host power) that requests CPU F-stop/P-state limits via the host interface. Reuse the measured
  min/max envelope. Tune KP/KD on hardware. Many deployments leave capping off → a low-priority
  feature.
- **Fan/thermal**: NOT iLO's policy (`docs/42`) — the rewrite only monitors + actuates + fail-safe.

## Open

- Exact ROM-CHIF message framing for Power-Regulator-mode set + the cap set (the `ROMCHIF_MSGID_*`
  payloads) — RE the `romchfservice_c.c` dispatch if BIOS-compatible power control is wanted.
- Default PID gains + the burn-test procedure (board-specific; measure on hardware).
- Whether Power Regulator is applied by iLO directly or purely a BIOS/RBSU setting iLO forwards.
