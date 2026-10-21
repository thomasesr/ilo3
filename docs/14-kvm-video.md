# iLO 3 KVM / video capture — `0xB0000000` buffer region + DVC pipeline

Date: 2026-10-20. Static RE of the app image (`decompiled/os_main_base0.bin`, `src\dvc*.c`,
`ARM:LE:32:v5`). This maps the `0xB0000000` device window flagged in `docs/13`.

## `0xB0000000`–`0xB3FFFFFF` (64 MB) = video-capture buffer region

Not a register block — it is the **framebuffer / capture-buffer memory** the ASIC fills with the
snooped host video. Evidence:
- The DVC buffer manager (`src\dvcbuf.c`) tracks **24 buffer descriptors** (16 B each) whose
  buffer addresses fall in `0xB1c37000`, `0xB2e8a000`, `0xB3037000`, `0xB3e91000`, … (scattered
  allocations across the 64 MB window). `Unable to locate capture buffer.`
- `Peer requests capture buffer 1` / `2` — double-buffered primary capture buffers.

## DVC = "Device Video Capture" — the KVM pipeline (software, in the app)

The iLO remote console (IRC = Integrated Remote Console) streams the host screen:

| Module | Role |
|--------|------|
| `src\dvccap.c` | **capture** — pull host frames into `0xB0000000` buffers |
| `src\dvcbit.c` | bit/tile **encode** (change-detect, run-length) |
| `src\dvcaes.c` / `src\dvcrc4sw.c` | **encrypt** the stream (AES / RC4-SW) |
| `src\dvcmux.c` | **mux** video + input channels |
| `src\dvcnet.c` / `dvcsnd` / `dvcrcv` | **network** send/receive to the IRC client (sockets, message queues) |
| `dvclic_task` | KVM **licensing** (iLO Advanced feature) |

Flow: `dvc_main` (`0x4014`) drives a capture state machine (`DVC_START`/`DVC_REFRESH`);
`Uploading buffer %d to IRC`, `Connection succeeded, diverting video stream`,
`Client requested KVM stream`. Keyboard/mouse come back from the client (`Received key from IRC`,
`dvcrcv`). Session sharing/takeover: `IRC-2 wants to acquire/share the session`.

## Capture-controller registers (RE pass 2)

Two ASIC register blocks drive video capture (both in the `0x01F0xxxx` peripheral window):

| Reg | Role | Evidence |
|-----|------|----------|
| `0x01F100C0` | **DVC capture control** base — `dvc_main` (`0x4014`) loads it after kicking capture | `ldr r7,=0x1f100c0` |
| `0x01F10088` | capture **status / frame** register (read into the capture context `+0x88`) | `0x1c32bc` |
| `0x01F10070`, `0x01F10098` | capture status/control (read) | `0x1c8a40`, `0x9e0f0` |
| `0x01F06000` / `+0x28` / `+0x2A` | **second video block** — host-video **source/snoop** registers (dumped in the capture debug) | `0x1c9918`-`0x1c9958` |
| `0x01F000DE` | video-related byte reg (0x01F00000 block) | `0x1c98f8` |

Software status: `dvc_main` writes `1` to the DVC control struct (`0x39f68`) `+0x4c`, then polls
`+0x4c` until the ASIC capture state is `≤ 0xF` (capture engine ready). The capture engine DMAs
snooped host frames into the `0xB0000000` buffers; the framebuffer addresses are the 16-byte
`dvcbuf` descriptors (buffers 1/2 double-buffered). Resolution/stride/exact buffer-base register
fields reach the hardware via software control structs (not single-literal MMIO), so enumerate
them on hardware; the two block bases (`0x01F100C0`, `0x01F06000`) and the status path ARE confirmed.

## Host-video source block `0x01F06000` (RE pass 3 — register widths/RW enumerated)

The second video block (`0x01F06000`, ~`0x34` bytes) is the **host-video snoop / digitizer**:
it reads the server's VGA output, detects the mode/timing, and feeds the capture engine. Register
map from access-pattern analysis (`0x1c9000`-`0x1ca4c0`):

| Off | W | R/W | Role (inferred) |
|-----|---|-----|-----------------|
| `+0x00` | 16 | r/w | **control / status** (polled often, written to arm) |
| `+0x02` | 16 | w | mode/command config |
| `+0x04` | 16 | r/w | timing config |
| `+0x06`,`+0x10`,`+0x12`,`+0x1E`,`+0x22`,`+0x24` | 16 | w | **video timing/mode** registers (H/V total, active, sync) |
| `+0x20` | 16 | r | **sync / mode status** (read to detect the host mode) |
| `+0x28` / `+0x2A` | 16 **& 32** | r/w | **indexed data port** — heavy read-modify-write (16- and 32-bit); indirect access to the host VGA register file / capture config |
| `+0x2C`,`+0x30` | 32 | r | data / counter (framebuffer addr or current scanline) |

The `+0x28` port dominates (dozens of RMW cycles) — the classic index/data pattern of a CRTC/VGA
snoop: step an index, read/modify/write the datum. So the block digitizes the host VGA, exposes
its timing via `+0x02..+0x24`, reports detected mode at `+0x20`, and the capture engine
(`0x01F100C0`, DVC) DMAs the resulting frames into the `0xB0000000` buffers. Exact per-index
field semantics (which index = H-total etc.) need hardware correlation.

## For the OpenBMC port

- `0xB0000000`–`0xB3FFFFFF` → a **reserved-memory** node (64 MB) for the capture framebuffers.
- Capture controller at `0x01F100C0` (Gromit-XE video-capture block) → needs a **custom KVM
  capture driver** feeding OpenBMC's video pipeline (e.g. a V4L2-style source for
  `obmc-ikvm`). This is substantial net-new work (like the ASPEED `aspeed-video` driver).
- The crypto/stream/licensing layers are HP-proprietary application code — replace with
  OpenBMC's VNC/`obmc-ikvm`, not port.

## Open

- ~~Enumerate the capture-controller registers~~ **PARTIAL**: two blocks found — DVC capture
  `0x01F100C0` (+status `0x01F10088`/`0x10070`/`0x10098`) and host-video source `0x01F06000`
  (+0x28/+0x2A). Exact resolution/framebuffer-base fields are via software structs — enumerate on HW.
- Host video source = the **`0x01F06000` block** (snoop/graphics registers, dumped alongside
  capture); confirm whether it taps VGA or a separate graphics path on hardware.
