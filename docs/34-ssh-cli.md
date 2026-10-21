# iLO 3 SSH + CLI (SMASH CLP) — RE for the clean-room rewrite

Date: 2026-10-21. Static RE of `os_main_base0.bin`. Maps the SSH server + command-line interface
so the Rust rewrite can reimplement it. SSH + SMASH CLP are **public standards** (build to spec);
only the HP `oemhp_*` targets/verbs need the binary as a feature reference.

## SSH server

- **Allegro RomSShell 4.62** (`SSH-2.0-RomSShell_4.62`) — same vendor as the RomPager web server.
- Protocols: SSH-2 (primary) + an **SSH-1.99 / SSH-1 fallback** (`SSH1-3DES`, `SSH1-BLOWFISH`) —
  **deprecated and insecure**; drop in the rewrite.
- KEX/auth: Diffie-Hellman, HMAC-SHA, ECDSA host keys (`HostKey`/`HOST_KEY`/`SSHKey`).
  Public-key user auth supported (`SSHKeyHandler`, `user_pkey_info` endpoint, `docs/33`).
- Listener: `SSHListener` / `SSHConnectionServer`.
- **Security note**: SSH-1 + 3DES/Blowfish + old DH are weak — part of why iLO 3 is risky on
  modern networks, and a core reason to rewrite. Use only modern KEX/ciphers in Rust.

## CLI = SMASH CLP (CIM-based) + HP extensions

The CLI is **DMTF SMASH CLP** (SM CLP) — a CIM object tree navigated by standard verbs; errors are
CIM (`CLI_CIM_ERR_*`). Each command = a `CLI_<name>Handler` + `Help` + `ContextHelp`.

**Standard SMASH CLP verbs**: `cd`, `show`, `set`, `create`, `delete`, `load`, `reset`, `start`,
`stop`, `help`, `exit`, `version`, `date`.
**HP/iLO extension verbs**: `power` (host power), `uid` (UID LED), `vm` (virtual media),
`vsp` / `vspclose` / `vsppassthrough` (virtual serial port), `textcons` / `textconspassthrough`
(remote text console), `nmi`, `uname`, `lang`, `dumpsessions`, `windbgenable`/`windbgdisable`
(Windows kernel-debug over VSP), `return`.

## Target tree (the CIM object model)

`cd`/`show`/`set` navigate these. Maps onto data/control already RE'd.

- **`/map1`** — the iLO management processor:
  `accounts1` (users), `config1`, `settings1`, `firmware1`, `nic1`, `enetport1`, `dhcpendpt1`,
  `dnsendpt1`, `dnsserver1-3`, `gateway1`, `snmp1`, `log1`, `blade`,
  `oemhp_dircfg` (directory/LDAP), `oemhp_license1`, `oemhp_sntp1`, `oemhp_sso`/`oemhp_ssocfg`
  (single sign-on), `oemhp_vm1` (virtual media).
- **`/system1`** — the host server:
  `cpu`, `fan`, `led1`, `drives`, `console1`, `bootconfig1`, `firmware1`, `bay`, …
- **`/lanendpt1/ipendpt1`** — LAN/IP endpoint.

Properties on these targets map to the sensor/power/network/user data paths (`docs/22`-`27`, plus
network/user stores to be RE'd in `docs/38`/`40`).

## Rust rewrite mapping

- **SSH**: **`russh`** (async Rust SSH-2) — modern KEX (curve25519), AES-GCM/ChaCha20, Ed25519/ECDSA
  host keys; **no SSH-1, no 3DES/Blowfish**. Public-key + password auth, on `smoltcp`.
- **CLI**: a small **SMASH CLP** engine — a CIM-like in-memory target tree (`/map1`, `/system1`)
  with `show`/`set`/`cd`/`reset`/`start`/`stop` + the HP verbs, each bound to HAL/service actions.
  Reuse the mapped data (sensors/power/users/network). The target list above is the contract.
- **Console passthrough** (`vsp`/`textcons`): the virtual serial port bridges to the host UART /
  CHIF — wire to the host-interface layer (`docs/28`); KVM text console ties to `docs/14`.

## Open

- Per-target **property names** (the `show` output fields) + `set` grammar — RE just-in-time per
  target (standard SMASH CLP property names + HP extensions).
- The VSP/textcons passthrough transport specifics (host serial over CHIF).
- User/account store layout (`/map1/accounts1`) → `docs/38`.
