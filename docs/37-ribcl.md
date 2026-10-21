# iLO 3 RIBCL (XML scripting) — RE for the clean-room rewrite

Date: 2026-10-21. Static RE of `os_main_base0.bin`. Maps HP's **RIBCL** (Remote Insight Board
Command Language) — the XML command/response scripting protocol. Proprietary but **publicly
documented** (HP "iLO 3 Scripting and Command Line Guide"); this RE confirms the command set
present in FW 1.94 so the Rust rewrite can reimplement the subset in use. Transport:
`dispatch_run_http_ribcl` on the XML port (`HTTP_PORT_XML`, `docs/33`), also via local
HPONCFG/`hponcfg`.

## Envelope

```xml
<RIBCL VERSION="2.0">
  <LOGIN USER_LOGIN="admin" PASSWORD="...">
    <RIB_INFO MODE="write">          <!-- one of the info blocks, read|write -->
      <GET_FW_VERSION/>              <!-- a command -->
    </RIB_INFO>
  </LOGIN>
</RIBCL>
```

Response: `<RESPONSE STATUS="0x0000" MESSAGE="No error"/>` (hex status; `SYNTAX ERROR` on parse
failure), with data elements for GET_* commands. Each **info block** is opened/closed
(`OPEN_*_INFO`/`CLOSE_*_INFO`) and carries a `MODE="read"|"write"`.

## Schema semantics (confirmed vs the public HP iLO 3 Scripting & CLI Guide)

- **`LOGIN`** — provides authentication (`USER_LOGIN`/`PASSWORD`); the user needs a valid iLO
  account with sufficient privilege. All info blocks nest **inside** a `LOGIN` block.
- **`MODE="read"|"write"`** — each info block requires it. **`write` = read + write** (editing
  allowed); **`read` = read-only** (modification rejected). Opening a block in write mode reads the
  relevant config DB into memory for editing; `CLOSE_*_INFO` commits.
- **`RESPONSE`** — `STATUS` is an error number (**`0x0000` = no error**), `MESSAGE`/`MSG` describes
  any error; `SYNTAX ERROR` on parse failure. GET_* commands return data elements alongside.
- Example: `GET_EMBEDDED_HEALTH` is issued inside a `SERVER_INFO MODE="read"` block.

This RE (FW 1.94 command set) matches the published guide — so reimplement to the **guide's schema**
(per-command attributes/elements), using this doc as the confirmed inventory.

## Info blocks (7+)

`USER_INFO` (accounts), `RIB_INFO` (iLO config: network, firmware, logs, certs, SNMP),
`SERVER_INFO` (host: power, health, boot, inventory), `DIR_INFO` (directory/LDAP),
`RACK_INFO` / `BLADESYSTEM_INFO` (blade/enclosure), `RM_INFO` (rack manager), `SSO_INFO`
(single sign-on). ~**180 commands** total; categories:

- **Read (`GET_*`)**: `GET_EMBEDDED_HEALTH` (the big one — fans/temps/PSU/VRM/drives),
  `GET_ALL_{FANS,TEMPERATURES,POWER_SUPPLIES,PROCESSORS,MEMORY,NICS,STORAGE,VRM,HEALTH_STATUS,
  FIRMWARE_VERSIONS,USERS,LICENSES,LANGUAGES}`, `GET_FW_VERSION`, `GET_ASSET_TAG`,
  `GET_HOST_POWER_REG_INFO`, `GET_DIAGPORT_SETTINGS`, `GET_CURRENT_BOOT_MODE`,
  `GET_SERIALNUMBER_INFO`, `GET_OA_INFO`, `GET_AHS_STATUS`, cert `GET_CERT_*`/`_REQ_INFO` (CSR).
- **Users**: `ADD_USER`, `DELETE_USER`, `MOD_USER`, `GET_ALL_USERS`, `ADD_GROUP`, `DELETE_GROUPS`.
- **Power / boot**: `SET_HOST_POWER` (on/off), `HOLD_PWR_BTN`, `PRESS_PWR_BTN`, `COLD_BOOT_SERVER`,
  `SERVER_AUTO_PWR`, `SET_HOST_POWER_SAVER`, `SET_POWER_CAP`, `SET_ONE_TIME_BOOT`,
  `SET_PERSISTENT_BOOT`, `SET_PENDING_BOOT_MODE`. → power control (`docs/24`).
- **Config (`MOD_*`)**: `MOD_NETWORK_SETTINGS`, `MOD_GLOBAL_SETTINGS`, `MOD_SNMP_IM_SETTINGS`,
  `MOD_DIR_CONFIG`, `MOD_SSO_SETTINGS`, `MOD_TWOFACTOR_SETTINGS` (2FA), `MOD_ENCRYPT_SETTINGS`,
  `MOD_DIAGPORT_SETTINGS`, `MOD_ENCLOSURE_IP_SETTINGS`, `HOTKEY_CONFIG`.
- **Logs**: `CLEAR_EVENTLOG`, `CLEAR_IML`, `CLEAR_LOGS`, `CLEAR_DIAGS_MEMORY_ERRORS`.
- **Virtual media**: `INSERT_VIRTUAL_MEDIA`, `EJECT_VIRTUAL_MEDIA`.
- **Misc**: `RESET_RIB`, `SET_ASSET_TAG`, `SET_LANGUAGE`, `SET_PERS_MOUSE_KEYBOARD_ENABLED`,
  `SET_AHS_STATUS`, `COMPUTER_LOCK_CONFIG`, **ERS** (`SET_ERS_DIRECT_CONNECT`/`_IRS_CONNECT`/
  `_WEB_PROXY` — Insight Remote Support), `SET_HOST_NVRAM`, `SET_EV`.

Nearly every `GET_*` maps onto data already RE'd (health → `docs/25`, power → `docs/20`/`24`,
inventory → `docs/22`, drives → `docs/23`); `SET_*`/`MOD_*` map onto the same control + the config
store (`docs/38`). RIBCL is an XML veneer over the same functions as the web/CLI/IPMI.

## Rust rewrite mapping

- A small **RIBCL handler**: XML parse (`quick-xml`), `LOGIN` auth (privilege bitmap, `docs/38`),
  dispatch by `(info-block, command)`, emit `<RESPONSE STATUS MESSAGE>` + data. Reuse the HAL/
  service data; the ~180-command list is the contract (implement the common subset first:
  GET_EMBEDDED_HEALTH, GET_FW_VERSION, GET_ALL_USERS, SET_HOST_POWER, MOD_USER, MOD_NETWORK_SETTINGS).
- **Strategy**: offer **Redfish** (modern, standard) as the primary scripting API, and keep a
  **minimal RIBCL** for legacy HPONCFG scripts — many shops still drive iLO 3 via RIBCL.
- Build to the **public HP scripting guide** schema + this confirmed command set; do not copy HP
  code (clean-room).

## Open

- Per-command **attribute/child-element schemas** (RE just-in-time per command from the scripting
  guide + observed responses).
- The full 180-command enumeration + which are no-ops/unsupported in FW 1.94.
- `hponcfg` (local RIBCL over a device, not the network port) if local scripting parity is wanted.

## Reference (public HP docs — the schema source)

- HP iLO 3 Scripting and Command Line Guide (RIBCL / RIB_INFO / base commands) — HPE support docs
  `a00018323`/`sd00002199`; mirrors on manualzz. Build the per-command schema from these + this
  confirmed command inventory (clean-room: use the published schema, not HP code).
