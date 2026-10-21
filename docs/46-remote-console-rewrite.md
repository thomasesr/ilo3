# iLO 3 remote console — open-source replacement for the EOL Java/.NET IRC

Date: 2026-10-21. Decision doc. The stock KVM client is dead tech: a JNLP/Java
Web Start applet that needs Internet Explorer, plus a Windows-only .NET ClickOnce
app (`IRC.application` v1.4.0.2092, docs/45). Both are unusable on a modern browser.
This picks the replacement for the Rust rewrite, sized to the hardware.

## The real constraint is the SERVER encode, not the client

Swapping the client is easy; the hard limit is that **this BMC encodes video in
software** — 250 MHz ARM926EJ-S (ARMv5, no SIMD), **no hardware video codec**
(docs/32), ~60 MB RAM. Contrast OpenBMC's reference KVM (`obmc-ikvm`), which leans
on the **AST2500's hardware JPEG engine** (V4L2 reads pre-compressed frames). We have
no such engine — the stock iLO already does software delta-tile encode (`dvcbit`,
docs/14) on this exact chip. So the replacement must keep encode cheap and accept
**low frame-rate, console-oriented** KVM (BIOS/POST/OS-install screens — mostly static
text), not 30 fps video.

## Decision: RFB (VNC) server on the BMC + noVNC in the browser

The modern open BMC-KVM stack, and the right fit here:

- **Protocol: RFB 3.8 (VNC)** — RFC 6143, a *public spec* → clean-room, implement
  directly in Rust (no HP code, no GPL lib needed). This is what `obmc-ikvm` speaks.
- **Browser client: noVNC** — HTML5 Canvas + WebSocket, **MPL-2.0**, actively
  maintained (v1.7.0, Apr 2026), **no plugin / no Java / no IE**, native **`wss://`
  (TLS)**. Runs in any current browser. Bundle the minified build in flash, serve it
  from the web UI. Replaces both dead applets outright.
- **Transport: native WebSocket over TLS** on the existing web-server port
  (`rustls`). noVNC connects directly when the server speaks WebSocket — **no
  `websockify` proxy, no separate 17990 socket, no bespoke AES/RC4** (drops the whole
  `REMOTE_CONSOLE_ENCRYPTION` + dvcaes/dvcrc4sw layer, docs/45). Auth = the existing
  web session cookie + Remote Console privilege (docs/38) — same model as today, minus
  the raw port.

### Encoding: Hextile (primary), ZRLE optional

| Encoding | Deps | CPU | Use |
|----------|------|-----|-----|
| Raw / CopyRect | none | trivial | always (fallback, scroll) |
| **Hextile** | **none** (16×16 tile RLE) | **lightest** | **primary** — no zlib |
| ZRLE / Tight | zlib (miniz) | heavier | optional, slow-link only |

Hextile needs **no zlib** — pure tile run-length, the cheapest real encoding — and is
the natural match for an ARMv5 software encoder (reference: Hextile ~8 ms vs ZRLE
~18 ms per simple-UI frame on a fast CPU; Hextile wins harder the slower the core).
Start Hextile-only; add ZRLE behind a feature flag if WAN bandwidth ever matters.

## It reuses the RE'd plumbing — RFB is a thin front-end

RFB maps cleanly onto subsystems already mapped; only the codec + socket are new:

| RFB message | Backed by | Doc |
|-------------|-----------|-----|
| FramebufferUpdate (Hextile) | DVC capture: host VGA snoop `0x01F06000` → `0xB0000000` buffer; dirty-rect already tracked | docs/14 |
| KeyEvent | inject into **virtual USB HID keyboard** (`usbkbd.c`) | docs/29 |
| PointerEvent | virtual USB HID mouse (absolute-pointer preferred — no cursor sync) | docs/29 |
| ClientCutText / ServerCutText | the clipboard channel (`wait_clipboard`) | docs/45 |
| SetEncodings / SetPixelFormat | server caps negotiation (standard) | RFC 6143 |

Power + virtual media stay **out of the KVM stream** — they move to the web/Redfish
API (power docs/24/43, vMedia docs/29), unlike the stock IRC which tunnelled them in
P84. Cleaner separation, smaller KVM surface.

## Alternatives considered (and why not)

| Option | Verdict |
|--------|---------|
| **VNC/RFB + noVNC** | ✅ chosen — public spec, pure-browser client, cheap Hextile encode, reuses our capture/HID |
| **MJPEG** (PiKVM/uStreamer style) | ❌ full-frame JPEG every frame = heavy software encode **and** 10–50× bandwidth; terrible for a slow core + static console |
| **H.264 / WebRTC** | ❌ needs a HW encoder we don't have; software H.264 on 250 MHz ARMv5 = impossible real-time |
| **SPICE + spice-html5** | ❌ heavy server + protocol (audio/USB-redirect); overkill, won't fit RAM/CPU |
| **Apache Guacamole** | ➖ client-side gateway (guacd) — still needs the BMC to speak VNC, so VNC is the core anyway; optional deployment aid, not a BMC component |

## Prior art: existing embedded VNC servers (why none drops in)

Checked the lean/embedded RFB servers before deciding to write our own:

| Project | What it is | Fit |
|---------|-----------|-----|
| **uVNC** (Adam Dunkels, Contiki/uIP) | "world's smallest" RFB server; demoed on a Commodore 64 (1 MHz 8-bit, 64 KB) | 📘 **reference only** — proves RFB is cheap (our ARM926 @250 MHz is ~250× a C64), but it *serves a small self-rendered screen*, not a captured host framebuffer: no dirty-tile encode over megapixel frames, no HID, no WebSocket/TLS. Reuse = gut it, rewrite in Rust. |
| **LibVNCServer** (used by `obmc-ikvm`) | full C VNC server lib, all encodings | 📘 reference — C/GPL, std/POSIX, assumes an OS framebuffer; too heavy for `no_std` Rust + license tension |
| **emVNC-Server** (SEGGER) | commercial low-footprint embedded RFB server | ❌ proprietary — fails the open-source requirement |

**Takeaway:** the RFB *protocol* was never the bottleneck (uVNC runs it on a potato);
the cost is **encoding a large captured host framebuffer**, which no tiny server does.
So we implement the RFB server ourselves (clean-room, RFC 6143) and spend the effort on
the Hextile encoder over the DVC capture. uVNC/LibVNCServer inform it; neither is a dep.

## Rust implementation notes

- **Implement RFB directly** (clean-room from RFC 6143) — the server state machine is
  small (handshake → SetPixelFormat/SetEncodings → FramebufferUpdateRequest loop).
  Reference crates to study, not necessarily depend on: `rustvncserver` (RFC 3.8,
  Hextile+ZRLE), `vncrs`. Most are `std`; the firmware hand-rolls the ~small protocol
  on `embassy`/`smoltcp` + `rustls`.
- **noVNC** served static from flash (minified `core/` + `app/`, a few hundred KB —
  trivial vs the .NET runtime dependency it replaces). Wire its WebSocket to the RFB
  task; TLS terminated by the web server (`rustls`).
- **Framebuffer source**: port `dvccap` capture to the Rust HAL (video-snoop
  `0x01F06000` → `MRDVCBUF` `0xB0000000`), expose dirty tiles to the Hextile encoder.
  This capture half is the genuinely hardware-specific work (docs/14, HW-confirm).

## Fit / budget

- **Flash/RAM**: RFB server + Hextile encoder + noVNC assets ≪ the old stack; no
  Java/.NET runtime, no zlib if Hextile-only. Fits the 8 MB flash / ~60 MB RAM.
- **Frame-rate**: software Hextile on ARMv5 ⇒ low FPS. Acceptable for a *management*
  console (BIOS/POST/recovery), which is the actual use. Set expectations: KVM for
  control, not for watching video.
- **Staging** (PLAN-rust.md): remote console is deferred behind the core BMC. Ship
  **VSP/SOL text console first** (cheap line protocol, docs/45) as the minimal remote
  console; add RFB/noVNC KVM when the capture HAL is brought up on real hardware.

## Summary

Drop the Java/IE applet + .NET IRC + the proprietary P84/AES socket. Replace with a
**clean-room RFB (VNC) server** feeding **noVNC over WebSocket+TLS** in any modern
browser, **Hextile** encoding to stay within the software-only ARMv5 budget, reusing
the already-RE'd capture (docs/14), HID (docs/29) and clipboard (docs/45) paths. Text
console (VSP/SOL) first; KVM video second.

## Client compatibility (one server, many viewers)

Because the BMC serves **standard RFB**, every VNC client interoperates — the server
is the only thing we build:
- **noVNC** (browser, `wss://` TLS) — **primary/default**, bundled in flash. No install.
- **Native VNC viewers** — TigerVNC, TightVNC, RealVNC, Remmina, UltraVNC Viewer —
  connect to the raw RFB port (over an SSH tunnel for encryption), a **bonus** power-user
  path, not the default. Windows-only ones (UltraVNC) don't replace noVNC.
- **uVNC / MicroVNC** (Dunkels) — a *server*, not a client, and too minimal to reuse
  (see Prior art above). Not involved in the shipped design.

Decision locked: **build a clean-room Rust RFB server; ship noVNC as the client.**
