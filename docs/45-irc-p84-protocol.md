# iLO 3 Integrated Remote Console — P84 streaming protocol + session layer

Date: 2026-10-21. Static RE of the app image (`decompiled/os_main_base0.bin`,
`src\dvcnet.c` / `p84.c` / `irccmd.c` / `dvcrcv.c` / `dvclic.c` / `dvcmux.c`,
`ARM:LE:32:v5`). Companion to **docs/14** (DVC capture pipeline + `0xB0000000`
framebuffer) and **docs/29** (vUSB/HID devices). docs/14 mapped capture→encode→
buffer; this maps the **network/session half** it left open: how a frame leaves
the box, how the client is authorised, and how keyboard/mouse/clipboard come back.

This is the HP-proprietary **"P84"** wire protocol — must be reimplemented from
RE'd behaviour (clean-room), never copied. Remote console is an **optional, heavy**
rewrite target (PLAN-rust.md): software video codec on a 250 MHz ARMv5 is the
hardest-to-fit feature; likely deferred behind the core BMC.

## What IRC is

**Integrated Remote Console (IRC)** = virtual KVM: streams the host VGA screen to a
client and injects keyboard/mouse back, plus virtual-power and virtual-media control
over one session. Three client forms (all served from iLO flash):
- **.NET IRC** — native Windows ClickOnce app (`IRC.application` **v1.4.0.2092**,
  HPE strong-name + Authenticode signed, COMODO RSA code-signing chain). Primary.
- **Java IRC** — `/html/java_irc.html?sessionKey=...` applet (JNLP/Web Start). Legacy.
- **Mobile app** — HP iLO mobile (`rc_info.mobileApp`).

Launch flow (web UI, `json_remconslock.c` + `rc_info.html`):
`/go/remcons` → applet URL `https://<ilo>/html/java_irc.html?sessionKey=<k>&addr=<ip>&port=<httpsPort>&lang=<l>`.
The **`sessionKey`** is the web-login session token (cookie) — it is how the raw
console socket is authorised (no separate console password; the key is validated
against the active web session). Privilege required: **Remote Console** bit
(`rc_info.remConsPriv`, privilege bitmap docs/38).

## Task topology (INTEGRITY tasks, from `CommonCreateTask` sites)

One IRC session spins up a set of cooperating tasks + message queues:

| Task (symbol)        | RTOS name      | Stack  | Role |
|----------------------|----------------|--------|------|
| `dvcnet_socket_task` | `dvcnet`       | 0x5000 | owns the IRC **TCP socket**; accept + stream I/O |
| `irccmd_p84_task`    | `irc_p84`      | 0x2000 | **P84 protocol** command handler / dispatch |
| `irccmd_tx_task`     | `irc_tx`       | 0x2000 | outbound frame pump |
| `irccmd_rx_task`     | `irc_rx`       | 0x2000 | inbound command pump |
| `dvcrcv_main`        | `dvcrcv`       | 0x3000 | **HID back-channel** (keyboard/mouse from client) |
| `p84_log_data`       | `p84_log_data` | 0x1000 | muxed sub-channel: log/telemetry data |
| `p84_pwr_mon`        | `p84_pwr_mon`  | 0x1000 | muxed sub-channel: host power-state monitor |
| `dvclic_task`        | `dvclic_task`  | 0x2000 | **license gate** + session shutdown |
| `wait_clipboard`     | `wait_clipboard`| 0x2000| clipboard sync channel |

Queues: `p84_queue` (128 msgs × 2 B, registered `P84_IRC_QUEUE`), `dvcp84_queue`,
`dvcrcv_queue`, `irc_clipboard_tx` (2 × 1024 B, `IRC_CLIPBOARD_TX`). The 2-byte
`p84_queue` element = a 16-bit P84 opcode/command token passed between tasks.

## Hardware transports (IODevices)

The DVC stack binds two INTEGRITY IODevices (both `RequestResource(..., "!systempassword")`):
- **`IODDPT`** (`p84_dev`) — "Display/Pointer Transport". `SynchronousReceive((Connection)p84_dev,0)`
  blocks for the next capture/event record; `WriteIODeviceRegister(p84_dev, 0xFFFFFFFF, 0x00ff0000)`
  programs it (mask 0xFFFFFFFF, value 0x00ff0000). This is the kernel-side bridge to the
  video-snoop / pointer hardware (host video source `0x01F06000`, docs/14).
- **`MRDVCBUF`** (`dviPhys`) — the DVC video **buffer** physical region (`0xB0000000`, docs/14).
  dvcbuf.c tracks the 24 capture-buffer descriptors here.

## Transport + session model

- **Raw TCP, own encryption — NOT TLS.** The console socket is a dedicated port,
  not the HTTPS port. The DVC stream carries its own cipher layer:
  - config key **`REMOTE_CONSOLE_ENCRYPTION`** ("Displays or modifies remote console
    encryption setting") — enable/force/disable the stream cipher.
  - cipher impl present both ways: **`dvcaes.c`** (AES) and **`dvcrc4sw.c`** (software RC4).
    AES is the modern path; RC4 the legacy/compat path. Key source + size = see wire
    section below (disasm).
- **Configurable ports** (NVRAM, docs/38): `TERMINAL_SERVICES_PORT` (the remote-console /
  "terminal services" listener, default **17990**), `Virtual Media listening at port %d`
  (default **17988**, docs/29). Observed port literals in the image: 17988, 17989, 17990,
  17998, plus **3002** (VSP/TEXTCONS) and 9300.
- **Authorisation**: the `sessionKey` from the web login (above). The socket task validates
  it against the live session table before streaming; no distinct console credential.

## Shared-console lock (`json/remcons_lock`, `json_remconslock.c`)

Remote console is single-active with a request-to-acquire model:
- GET/POST `json/remcons_lock`, method `apply_remcons_lock`, field **`lock_option`**.
  Observed enum: `1` (srvLockWin — automatic/takeover), `3` (srvLockDis — disabled/locked);
  `2` = manual-approve (by position). Exact enum = the json handler (confirm on HW).
- Preempt dialogue: `"IRC-2 wants to acquire the session in progress"` /
  `"Permission to acquire this session has been denied."` → when a second client asks,
  the holder is prompted to yield. `require_trust` POST = optional trusted-cert requirement
  for IRC (`ircReqTrustCert`).

## Clipboard channel (`wait_clipboard` task, `irc_clipboard_tx` queue)

Bidirectional text clipboard sync with the .NET client (Windows
`ChangeClipboardChain`/`OnClipboardChanged` semantics mirrored host-side):
`PostClipboard` / `"Received clipboard: %d bytes"`; guarded by `"Clipboard buffer too
large"` / `"Clipboard packet discarded."`. A discrete P84 sub-channel, 1024-B buffers.

## Licensing gate (`dvclic.c`)

IRC + virtual media are **iLO 3 Advanced License** features (also "iLO 3 Advanced
License for BladeSystem"). `dvclic_task` enforces it; on an unlicensed box the console
is refused. Web mirrors: `license_features` cache → `vm_applet`, `vm_script`,
`powerreg` flags; `"does not have Virtual Media license (local media via IRC)"`. For a
clean rewrite the license gate is **dropped** (owner's own hardware) — implement the
feature, not the paywall.

## Related: VSP (virtual serial) — separate module, same console family

Not P84/DVC but the other half of "remote console": **`.vsp.elf`** is a separate
INTEGRITY ELF address space (`.vsp.elf.text/.data/.heap/.stack`). Text console
**TEXTCONS** + **SOL** (Serial-over-LAN) bridge: `oemhp_vsp1` CLI target
(`/system1/oemhp_vsp1`), `VSP2SOL_Write`, `SOLRespServiceRequest`, `sol_connect`,
default **port 3002** (shared with `windbg_enable`). `ESC(` exits to CLI. Documented
here for completeness; its own rewrite = a small line-protocol over the host UART/CHIF,
far cheaper than KVM. (IPMI SOL path cross-ref docs/36.)

## DVC data pipeline (network side) — how a frame reaches the client

Per-frame path (modules confirmed by `src\*.c` names + docs/14):
1. **`dvccap.c`** — capture: ASIC snoops host VGA (`0x01F06000`) into an `MRDVCBUF`
   buffer (`0xB0000000`, double-buffered: "Peer requests capture buffer 1/2").
2. **`dvcbit.c`** — encode: proprietary bit/tile codec over the captured buffer
   (`dvcbit_put(len)`, length-checked). Delta/dirty-rect compression (exact codec =
   disasm/HW).
3. **`dvcmux.c`** — mux: interleaves the sub-channels (video / `p84_pwr_mon` power /
   `p84_log_data` log / clipboard / HID) onto the one P84 stream.
4. **`dvcaes.c`/`dvcrc4sw.c`** — encrypt the muxed stream (`REMOTE_CONSOLE_ENCRYPTION`).
5. **`dvcnet.c`** — frame + send over the TCP socket (`dvcnet_socket_task`,
   `dvcnet_handle_busy` back-pressure).
Reverse (client→iLO): `dvcnet` recv → P84 demux → **`dvcrcv.c`** (`dvcrcv_main`)
decodes HID + control → injects keyboard/mouse into the **virtual USB HID** (docs/29,
`usbkbd.c`) and routes power/vmedia commands to their managers.

## Wire format (P84) — frame + opcodes

> **Deferred — not on the critical path.** The rewrite adopts RFB/VNC (docs/46) and does
> NOT reimplement P84, so the exact frame bytes + opcode table are legacy-reference only.
> Partial disasm established: the `p84_queue` element is a **16-bit opcode** (`p84: %04d:
> 0x%04x at %x` = seq, opcode, addr) and there are **~18 outbound command builders** writing
> a command code to header `+0`. Full table left unfinished (agent hit API session limit);
> finish only if the legacy .NET/Java IRC must interoperate — otherwise skip.

## Clean-room rewrite plan (optional / deferred)

> **Replacement decided — see docs/46.** The rewrite does NOT reimplement P84: it drops
> the Java/.NET clients + the proprietary AES/RC4 socket and adopts **RFB (VNC) + noVNC**
> over WebSocket/TLS, Hextile encoding. This section records the legacy-mapping rationale.

Remote console is the heaviest feature on this SoC; stage it last, behind the core BMC.
- **Transport**: a dedicated TCP listener (`smoltcp`), port from NVRAM (default 17990),
  authorised by the shared web-session token — not a second credential store.
  **Modernise**: wrap the stream in **`rustls` TLS** instead of the bespoke AES/RC4 +
  `REMOTE_CONSOLE_ENCRYPTION` toggle; drop RC4 entirely. A new client is required anyway
  (the .NET/Java applets are EOL), so the wire protocol is ours to redefine — keep the
  P84 *channel model* (video/HID/power/clipboard mux) but carry it over TLS + a clean
  framing (e.g. length-prefixed CBOR or a small binary header).
- **Codec**: the hard part. The HP `dvcbit` tile codec is proprietary; a rewrite should
  target a standard instead — JPEG-tile or a simple RLE/delta-tile, matched to what a
  250 MHz ARMv5 can encode in software (no HW codec). Budget: this likely caps frame-rate
  hard; consider "on-demand / low-FPS" console only.
- **HID + power + clipboard + vmedia** reuse the same subsystems already mapped
  (docs/29 vUSB, docs/24 power, docs/43 regulator) — the console is a thin front-end.
- **License gate**: removed.
- **VSP/SOL**: implement first (cheap) as the minimal "remote console"; defer KVM video.

## Open gaps (need disasm / dynamic capture / HW)

- Exact P84 frame header bytes + the opcode/command table (pending agent).
- Cipher **key/IV derivation** (from `sessionKey`? negotiated?) and AES key size/mode.
- `dvcbit` codec internals (tile size, delta scheme, any JPEG fallback — `.jpeg` seen).
- `lock_option` full enum + the acquire/preempt handshake exact messages.
- HID packet layout (dvcrcv → usbkbd) and the pointer (absolute vs relative) encoding.
- Handshake/capability/version exchange right after `accept()` (if any).
