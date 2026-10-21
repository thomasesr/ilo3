# iLO 3 NVRAM config store + user/account DB — RE for the clean-room rewrite

Date: 2026-10-21. Static RE of `os_main_base0.bin`. Maps the persistent configuration store and
the user/account database — the foundation everything else persists through. (Covers both the
PLAN-rust `docs/40` NVRAM and `docs/38` users items.)

## NVRAM configuration store

- **Named key-value store** in NVRAM/EEPROM — `NvramDisk` (a small filesystem-like store),
  `EEPROM`, `PERSIST`/`PERSISTENCE`.
- **Config API**: `GET_CONFIG`, `CFG_READ` / `CFG_WRITE`, keyed by **name** (`CFG_BAD_NAME` on
  miss), with per-key **privilege** (`CFG_ILO_PRIV`), **size** (`CFG_SIZE`), a setting id
  (`CFG_BAD_SID`), and change notification (`CFG_CHANGE`). Backing struct `CFG_DATA_STRUCT`,
  `Settings_table`.
- **Keys** are the UPPERCASE setting names seen across the subsystems, e.g.:
  `SNMP_ADDRESS_1/2/3`, `SNMP_*_ROCOMMUNITY`/`TRAPCOMMUNITY`, `SNMP_ACCESS_ENABLED`,
  `ALERTMAIL_SMTP_SERVER`/`_PORT`/`_EMAIL_ADDRESS`/`_SENDER_DOMAIN`, network (static IP/DNS/WINS),
  `LOGIN_DELAY`, `HTTP_PORT`/`HTTP_PORT_XML`, `HOST_KEY`/`SSH_KEY`, etc. The web API
  (`json/*_settings`, `docs/33`) and CLI (`/map1/*`, `docs/34`) read/write these keys.
- Each key carries an access privilege → ties to the user privilege bitmap below.

## User / account database

- **Up to 12 local user accounts** (`MAX_USERS`, "12 users").
- **Account record fields** (from the `Accounts_*` CLI/handler ops): **login name** +
  **username** (`Accounts_nameSet/Show`, `Accounts_usernameSet/Show`), **password**
  (`Accounts_passwordSet/Check`), **group/privileges** (`Accounts_groupSet/Show`), **SSH public
  key** (`Accounts_sshkeyLoad/Show/Delete`). `Accounts_StarShow/Delete` = wildcard/all.
- **Passwords: bcrypt-hashed** (`bcrypt` + salt `Salta`; `passwd_set`/`passwd_get`/`passwd_server`,
  `CREDENTIAL`). bcrypt is still acceptable; the rewrite may use **argon2** for new hashes +
  bcrypt verify for migration.
- Login: `LOGIN_PRIV`, `LOGIN_DELAY`, `LOGIN_FAILED` (lockout/delay on failure).

## Privilege model (the `PRIV_BOX` bitmap)

Per-user privilege bits (the iLO privilege box):

| Bit | Meaning |
|-----|---------|
| `LOGIN_PRIV` | login to iLO |
| `REMOTE_CONS_PRIV` | remote console (KVM) |
| `VIRTUAL_MEDIA_PRIV` | virtual media |
| `RESET_SERVER_PRIV` | virtual power + reset |
| `CONFIG_ILO_PRIV` (`CONFIG_RIB/RILO_PRIV`) | configure iLO settings |
| `ADMIN_PRIV` / `UserAdmin` | administer user accounts |

Extended/role: `VIEW_LOGS_PRIV`, `CLEAR_LOGS_PRIV`, `DIAG_PRIV`, `EMS_PRIV`, `CONFIG_RACK_PRIV`,
`DIR_GRPACCT_PRIV` (directory group accounts), `UPDATE_ILO_PRIV`. The web UI exposes the core set
as `admin_privs`/`oper_privs`/`user_privs.{vmedia,...}` (`docs/33`). Directory (LDAP) users get
privileges via group roles (`oemhp_dircfg`, `directory_groups`).

## Firmware Update Manager (FUM) — related persistence

`fum_flash`/`fum_inventory`/`fum_flash_status`/`fum_count`/`fum_legacy_flash_thread` manage iLO
self-flash + component inventory. The config store survives firmware updates (settings preserved).
Flash layout: boot ROM/image (`docs/01`,`03`) + a config/NVRAM region; exact NVRAM flash offset
not yet pinned (stored via `NvramDisk`, abstracted).

## Rust rewrite mapping

- **Config store**: a Rust key-value store in a flash NVRAM region with **wear-leveling** +
  atomic/journaled writes (e.g. a log-structured store). Keep the key names for migration, or a
  clean typed schema with a migration shim. Per-key access = the privilege bitmap.
- **User DB**: a Rust account store (≤12): name/username/**argon2**(new)+bcrypt(migrate)/
  **privilege bitmap**/SSH pubkeys. Login delay/lockout on failure.
- **Privilege bitmap**: a Rust bitflags type (the 6 core + extended bits). Enforce on every API
  (web/CLI/IPMI/SNMP) + per config key.
- **Directory auth**: optional LDAP client mapping group → privilege (defer; `oemhp_dircfg`).

## Open

- Exact **NVRAM flash region** offset/size + the on-flash record format (RE `NvramDisk` /
  `CFG_WRITE` to pin the layout — needed if migrating existing config, optional for a clean store).
- The complete config-key list + each key's type/size/privilege (enumerate `Settings_table`).
- bcrypt cost factor used (for migration verify).
