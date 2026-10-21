# iLO 3 standard network + directory services — RE for the clean-room rewrite

Date: 2026-10-21. Static RE of `os_main_base0.bin`. Catalogs the remaining standard-protocol
services (NTP, SMTP, syslog, LDAP/directory, SSO, 2FA, WS-Man, discovery). All are **public
standards** — the rewrite builds them from RFCs + Rust crates; this RE only confirms which are
present and their config/modes. Config lives in the NVRAM store (`docs/38`); exposed via web/CLI/
RIBCL/SNMP.

## Time — SNTP (RFC 4330)
- `SNTPService`/`SNTPServer`/`SNTPCallback`; **primary + alternate** server (`SNTP_ALT_*`). Feeds
  the software clock (no hardware RTC, `docs/18`). Config `network_sntp`/`oemhp_sntp1`.
- **Rust**: an SNTP client (`sntpc`-style) → sets the system time; the software clock is seeded
  from here (critical — no HW RTC).

## Email alerts — SMTP (AlertMail)
- `AlertMailService`: `ALERTMAIL_SMTP_SERVER`/`_PORT`, `_EMAIL_ADDRESS`, `_SENDER_DOMAIN`, `_ENABLE`;
  `HELO`. Sends health/event alerts by email (second alert channel alongside SNMP traps, `docs/35`).
- **Rust**: `lettre`-style SMTP client fed by the EventBus (`docs/39`).

## Remote logging — syslog (RFC 3164/5424)
- `REMOTE_SYSLOG`: `syslog_server`, `syslog_port`, `syslog_enable`, `FACILITY`. Forwards iLO/IML
  events to a remote syslog server.
- **Rust**: a small syslog client off the EventBus.

## Directory auth — LDAP + Kerberos
- `DIR_*`: `DIR_ENABLE`, `DIR_AUTHENTICATION`, `DIR_SERVER`, `DIR_USER`, `DIR_ENTRIES`,
  `DIR_GRPACCT` (group accounts), **`DIR_KERBEROS`** (Kerberos auth), **`DIR_LOCAL`** (schema-free
  / local role mapping), **AD** (Active Directory). Two modes:
  - **HP extended schema** (directory objects extend the schema), and
  - **Schema-free / "local"** (map directory **groups → iLO privilege roles** locally — `DIR_LOCAL`
    + `directory_groups`, `docs/33`).
- Group → privilege mapping uses the privilege bitmap (`docs/38`).
- **Rust**: an LDAP client (`ldap3`-style, over `rustls`) + optional Kerberos; map group DNs to the
  privilege bitflags. Schema-free mode first (simpler, most common).

## Single sign-on — HP SSO
- `SSO_INFO`/`SSO_SERVER`/`SSO_SETTINGS`: trusted-server SSO (HP SIM / Onboard Administrator log in
  to iLO without re-auth, via a trust certificate). RIBCL `MOD_SSO_SETTINGS` (`docs/37`).
- **Rust**: trust-cert validation + a role map; low priority (HP-tooling feature).

## Two-factor / smartcard auth
- `TWOFACTOR_ENABLE`/`TWOFACTOR_SETTINGS`, `SmartcardLogin`: **X.509 client-certificate / smartcard
  (CAC)** login. RIBCL `MOD_TWOFACTOR_SETTINGS`.
- **Rust**: TLS client-cert auth in `rustls` (mTLS) + map cert subject → user. Good modern feature
  to keep.

## WS-Management (CIM over SOAP)
- `WS-Man` present — the legacy CIM/SOAP management API. **Low value today**; the rewrite should
  prefer **Redfish** and likely **drop WS-Man** (RE only if a specific tool needs it).

## Discovery — mDNS + SLP
- `mDNS` (Bonjour, `_http._tcp` etc.) + **SLP** (Service Location Protocol). Let management tools
  find the iLO on the network.
- **Rust**: an mDNS responder (`simple-mdns`-style); SLP optional.

## Rust rewrite mapping (summary)

| Service | Standard | Rust crate (indicative) | Priority |
|---------|----------|-------------------------|----------|
| SNTP | RFC 4330 | sntpc | **high** (seeds the clock — no HW RTC) |
| SMTP alerts | RFC 5321 | lettre | med |
| syslog | RFC 5424 | hand-rolled / `syslog` | med |
| LDAP+Kerberos | LDAP v3 | ldap3 + rustls | **high** (enterprise auth) |
| SSO | HP proprietary-ish | trust-cert + role map | low |
| 2FA / smartcard | X.509 mTLS | rustls client-auth | med (modern plus) |
| WS-Man | DMTF WS-Man | — (prefer Redfish) | drop |
| mDNS / SLP | RFC 6762 / 2608 | simple-mdns | low |

All are build-to-spec; none need deep binary RE beyond the config/mode notes above. Config keys →
the NVRAM store (`docs/38`); events → the EventBus (`docs/39`).
