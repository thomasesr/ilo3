# iLO 3 web server + JSON API — RE for the clean-room rewrite

Date: 2026-10-21. Static RE of `os_main_base0.bin`. Maps the management web surface so the Rust
rewrite can reimplement the API contract. Per the clean-room rule: HTTP/TLS are standards (build
from RFCs + Rust crates); the **endpoint list + JSON shapes + RIBCL schema** are what to RE.

## Server stack

- **HTTP(S) server: Allegro Software RomPager** (`allegro_main`/`allegro_server`/`allegro_uninit`,
  `RomPager`, `Webserver`) — a commercial embedded web server. HTTP/1.1, chunked, Basic/digest
  auth (`WWW-Authenticate`).
- **TLS: Allegro SSL** (OpenSSL-compatible API, `docs/32`) — software crypto, slow on 250 MHz.
- **Ports**: `HTTP_PORT` (redirect/HTTPS), `HTTP_PORT_XML` (RIBCL XML scripting), configurable.
- **Static UI**: the Vue-era-predecessor web UI (HTML/JS/CSS incl. jQuery/jQuery-UI) is embedded
  in the image (LZSS-compressed), served by RomPager.
- **Routers**: `dispatch_json` (the JSON REST-ish API) and `dispatch_run_http_ribcl` /
  `dispatch_get_http_ribcl_output` (RIBCL XML, on the XML port). JSON responses built via a typed
  builder (`JSON_TYPE_ARRAY` / `JSON_TYPE_METHOD` / `JSON_TYPE_STRING`).

## JSON API — 58 endpoints (`GET json/<name>` read, `POST json/<name>` set/action)

The client calls `sendJsonRequest("<name>", "GET"|"POST", "json/<name>", body, cb)`. The 58
endpoints, grouped:

- **Overview / status**: `overview`, `masthead`, `ilo_status`, `health_summary`, `version_info`,
  `v`, `diag_info`, `dbug_info`, `rck_info`, `ui_events`.
- **Health / sensors**: `health_fans`, `health_temperature`, `health_power_supply`, `health_vrm`,
  `power_supplies`, `drives_status`, `uid_status`.
- **Power**: `host_power`, `power_cap`, `power_capabilities`, `power_readings`, `power_summary`,
  `power_history`, `power_history_fast`, `power_regulator`, `power_misc`, `power_alert`.
- **Inventory**: `proc_info`, `mem_info`, `nic_info`, `boot_order`.
- **Network**: `network_general`, `network_ipv4`, `network_ipv6`, `network_sntp`, `timezone`.
- **Security / certs**: `certificate`, `csr`, `require_trust`, `access_settings`, `sso_settings`,
  `snmp_settings`, `directory_groups`, `directory_test`.
- **Sessions / auth / users**: `login_config`, `login_session`, `session_info`, `active_sessions`,
  `user_info`, `user_pkey_info`, `hot_keys`.
- **Logs**: `ilo_event_log`, `server_event_log`.
- **Licensing**: `license`, `license_features`.
- **Remote console / media**: `remcons_lock`, `vm_status`, `flash_status`.

Most endpoints map directly onto data paths already RE'd: health_* → sensors (`docs/25`),
power_* → PSU/power (`docs/20`,`24`), proc/mem/nic → inventory (`docs/22`), drives → backplane
(`docs/23`). The per-endpoint JSON field shapes are RE'd just-in-time when implementing each (the
client JS field names, e.g. `last_avg_pwr_accum`, are the schema hints).

## RIBCL — XML scripting interface (HP-proprietary)

A separate listener task (`RIBCL_TASK_LISTEN_NAME`/`_PRIORITY`/`_STACK_SIZE`) on `HTTP_PORT_XML`
handles **RIBCL** (Remote Insight Board Command Language) — HP's XML command/response protocol
(`dispatch_run_http_ribcl`). Proprietary; RE its command/response schema separately → `docs/37`.

## Certificate / key management

`json/certificate` + `json/csr` back a cert workflow: **CSR generation** (`CSR`, `CSRCallback`,
`CERT_FQDN`, `CERT_OWNER_SUBJECT`, `CERT_OWNER_SAN`, `CERT_SIGNATURE_ALGORITHM`), **import**, and a
self-signed default. Store = `CERT_STORE` (`CERT_STORE_LOCK`), formats PKCS1/7/12 + PEM (`docs/32`).
Operations: `CERT_SIGN`, `CERT_VERIFY`, `CERT_CHAIN`, `CERT_GET_*`.

## Rust rewrite mapping

- **Server**: Rust async HTTP — `picoserve`/`hyper`-style on `smoltcp` + **`rustls`** (replaces
  RomPager + Allegro SSL). Keep the web surface minimal (RAM/flash/CPU budget).
- **API**: reimplement the `json/<name>` endpoints as handlers over the HAL/service data — the
  58-name list above is the contract. Start with a core subset (overview, health_*, power,
  host_power, network, sessions) and grow.
- **Static UI**: ship a tiny modern SPA (or server-rendered pages) instead of the embedded jQuery
  UI — far smaller.
- **TLS certs**: a Rust cert store (PEM/PKCS8) + CSR generation (`rcgen`), self-signed default.
- **RIBCL**: optional — reimplement the subset in use, or offer a minimal Redfish instead
  (modern, standard). RE the schema in `docs/37` if RIBCL compatibility is wanted.

## Open

- Per-endpoint JSON request/response schemas (RE just-in-time per endpoint; client JS = the hint).
- The RomPager auth flow (Basic/digest + session cookies/tokens) → `docs/38` (auth/users).
- RIBCL command/response grammar → `docs/37`.
