# iLO 3 SNMP + alerting — RE for the clean-room rewrite

Date: 2026-10-21. Static RE of `os_main_base0.bin`. Maps the SNMP agent + alerting so the Rust
rewrite can reimplement it. SNMP + the CPQ MIBs are **public** (HP publishes the MIB files; OIDs
under `1.3.6.1.4.1.232` = Compaq/HP enterprise) — build to spec and wire the OIDs to our data.

## SNMP agent

- **SNMP v1 / v2c only** (community-based). **No v3** — no USM/engineID/authPriv in the image.
  `SNMPService`, `SNMPInfo`, `SNMPSettings`, `GetRequest`, `Community`. (The rewrite should **add
  v3** for modern auth/priv — the original's v1/v2c is a weakness.)
- **Config**: up to **3 destinations** `SNMP_ADDRESS_1/2/3`, each with `_ROCOMMUNITY` (read
  community) + `_TRAPCOMMUNITY`; `SNMP_ACCESS_ENABLED`, `SNMP_ALERTS`, `FWD_SNMP_ALERTS` (forward
  host-OS SNMP through iLO). Settings via `json/snmp_settings` (`docs/33`) and `/map1/snmp1`
  (`docs/34`).

## MIBs exposed (Compaq/HP enterprise, OID base `1.3.6.1.4.1.232`)

- **`CPQRIB3`** — Remote Insight Board MIB v3 = **the iLO SNMP MIB** (cpqSm2-ish): iLO status,
  config, events.
- **`CPQCIDRV`** — Compaq CI driver MIB.
- **cpqHealth / cpqHost** — health + host info (fed from the `eh_*` health/SDR framework).
- Others: `CPQRFBLO`, `CPQTAF`, `CpqKey`. Data for all of these = the already-RE'd sensor/power/
  PSU/inventory paths (`docs/20`,`22`-`27`).

## Traps / alerts

Event source = the **`eh_*` event-handler/health framework** (sensors, power, PSU, IML). Trap
categories (string evidence):

- **Cold start**: `COLD_START_TRAP_BROADCAST`.
- **Power**: `PWRALERT` (`_ARMED`/`_FIRED`/`_NEVER_ISSUED`, `PWRALERT_SETTINGS`,
  `PWRMGR_API_GET/SET_POWER_ALERT_SETTINGS`); "Power outage", "power on failed, not enough power",
  "manual override engaged", "power consumption and reclaim".
- **Host OS**: `HOSTTRAP`, `OS_TRAPS`, `FWD_SNMP_ALERTS` (forward host-OS SNMP traps).
- **Health / log**: `EH_ALERT_IML_CLEARED` (IML cleared), general health alerts; `LOG_RACK_ALERTS`
  (rack/blade).
- **Test**: `Test Alert` / `Test Trap`.
- Trap queue: `SNMPTRAP_MAX_ALERTS_QUEUED`, `SNMPTRAP_RECVQ_NODE`; failures `SNMP_ALERT_FAILED`.

## Second alerting channel — email (SMTP)

`AlertMailService`: iLO also sends **email alerts** over SMTP — `ALERTMAIL_SMTP_SERVER`/
`ALERTMAIL_SMTP_PORT`, `ALERTMAIL_EMAIL_ADDRESS`, `ALERTMAIL_SENDER_DOMAIN`, enable flag. Same
event source as the SNMP traps.

## Rust rewrite mapping

- **Agent**: a Rust SNMP agent (v1/v2c for compat, **+ v3 USM/auth-priv for security**). Load the
  **public CPQ MIB files** (cpqhlth, cpqsm2/cpqrib, cpqcidrv…) and bind the OIDs to the HAL/service
  data (sensors/power/inventory). A `no_std`-friendly SNMP crate or a small hand-rolled agent.
- **Traps**: emit on health/power events from the event framework; up to N destinations with
  per-dest community; include the CPQ trap OIDs. Host-OS SNMP forwarding = a UDP relay.
- **Email**: an SMTP client (`lettre`-style) for the AlertMail channel.
- **Config**: expose via the web API + CLI (`/map1/snmp1`), stored in NVRAM (`docs/40`).

## Open

- Exact **CPQ trap OIDs + varbinds** per event (from the public CPQ MIB files, not the binary).
- Which cpqHealth tables iLO populates vs leaves empty (RE just-in-time per table).
- The event→trap mapping table in the `eh_*` framework (which internal events fire which traps).
