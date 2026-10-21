# iLO 3 event logs (IML, iLO log, AHS) — RE for the clean-room rewrite

Date: 2026-10-21. Static RE of `os_main_base0.bin`. Maps the logging subsystem — the last
app-layer piece. Three logs, one event framework.

## The event framework (source of all logs + alerts)

Events flow through `EVENT_RAISE` → `EVENT_QUEUE` → `EVENT_SERVICE` (the `eh_*` handler). A raised
event is **logged** (IML/iLO log), **trapped** (SNMP, `docs/35`), and optionally **emailed**
(AlertMail). Event types `EVT_*` (dozens), e.g.: `EVT_ASR`/`EVT_EASR` (Automatic Server Recovery),
`EVT_HOSTPWR_TRANSITION_{ON,OFF,RESET}`, `EVT_HOSTPWR_ON_LEVEL`, `EVT_HEALTH_CHANGE`,
`EVT_HEALTH_SDR_ARRIVED`, `EVT_FRU_ASSET_TAG_CHANGE`, `EVT_CLOCK_CHANGE`, `EVT_BOOT_COMPLETE`,
`EVT_BIST`, `EVT_ILO_FLASH_END`, `EVT_EXPBUS_INT_BUTTON`.

## Record format (coalescing)

Two record types, same fields: **`SystemRecord`** (IML / server hardware) and **`MapRecord`**
(iLO event log). Fields (from `*_Show` accessors):

| Field | Meaning |
|-------|---------|
| `number` | sequential event id |
| `severity` | **INFORMATIONAL / CAUTION / CRITICAL / REPAIRED** (+ `CORRECTED`/`UNCORRECTABLE` for memory) |
| `date` / `time` (`INITIAL_UPDATE` + last) | first-seen + last-seen timestamp |
| `UPDATE_COUNT` | repeat count — **duplicate events coalesce** (increment count, not re-log) |
| `description` | human text (class/code → string) |
| (iLO log) `Event_UsernameIPAddress` | who/where, for iLO events (logins, config changes) |

Severity lifecycle: a Caution/Critical event can later be marked **Repaired**.

## The three logs

1. **IML** (Integrated Management Log) — **server hardware** events (POST, memory corrected/
   uncorrectable, thermal, power, ASR, FRU). `SystemRecord`s. `IMLOC`, `iml_legacy`, `ImlEmpty`,
   `iml_clr`. This is the HP server event log (also surfaced as IPMI SEL, `docs/36`).
2. **iLO event log** — **iLO-side** events (logins, config changes, flash, session). `MapRecord`s
   with user/IP.
3. **AHS** (Active Health System) — continuous **binary telemetry** for HP support (not a
   human-readable event log). `AHS_STATUS`, `AHS_CLEAR_DATA`, `AHS_COUNTERS`, `AHS_FACTORY_STATUS`;
   downloaded as an `.ahs` bundle.

## Storage

Flash-resident, **circular / wrapping** (`LOGlobal.flash`/`isFlash`, `WRAP`) — oldest records drop
when full. Coalescing keeps it compact. Survives reboots; cleared via `CLEAR_IML`/`CLEAR_EVENTLOG`
(RIBCL `docs/37`) or the web/CLI.

## Exposure (all interfaces already mapped)

- Web: `json/server_event_log` (IML) + `json/ilo_event_log` (`docs/33`).
- CLI: `/map1/log1` (`docs/34`).
- RIBCL: `CLEAR_IML` / `CLEAR_EVENTLOG` + GET (`docs/37`).
- IPMI: the IML ↔ **SEL** (`docs/36`).
- SNMP: traps on raised events (`docs/35`); `EH_ALERT_IML_CLEARED`.

## Rust rewrite mapping

- A **coalescing, circular log store** in a flash region: records `{number, severity, class/code,
  first_ts, last_ts, count, description, actor(user/ip)}`. On raise, match an existing open record
  (same class/source) → bump `count`+`last_ts`, else append; wrap on full. Wear-leveled writes.
- Two logical views (IML = hardware, iLO = management) over one store, or two stores.
- Expose the store through all interfaces (web/CLI/IPMI-SEL/RIBCL/SNMP) — one record model.
- **AHS**: optional/deferred (binary HP-support telemetry) — skip for a minimal BMC, or a simple
  ring of telemetry samples if wanted.
- Event framework = a Rust `EventBus` (raise → {log, snmp-trap, email}) fed by sensors/power/health.

## Open

- The exact on-flash record binary layout + the IML class/code → description table (RE
  just-in-time; standard HP IML classes, many → `EVT_*` above).
- AHS binary format (only if AHS compatibility is wanted — unlikely for a clean rewrite).
