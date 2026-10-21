# iLO 3 vUSB / virtual-media bridge — architecture

Date: 2026-10-20. RE of `src\uhci.c`, `usbservice_c.c`, the `vm_*` virtual-media manager
(`os_main_base0.bin`) + the kernel BSP UHCI IODevices (`kernel_type2.bin`). This is the
host-facing virtual USB path: how iLO presents a keyboard, mouse, and removable media to the
server as real USB devices. It is the **largest net-new subsystem** of an OpenBMC port.

## Correction to docs/06

`eh_intf` is the **Event Handler interface** (`eh_intf_daemon/dispatcher/service`,
`eh_intf_process_memory_events`) — it posts *health/IML events* to the host via `chif_post`,
NOT virtual media. docs/06's "vMedia via eh_intf_submit → chif_post" was a misattribution. The
vMedia/vUSB bridge is `usbservice_c.c` + `uhci.c` + the `vm_*` manager.

## What the host sees (PCI fn .4, `103c:3300`, UHCI, docs/06)

A standard **UHCI (USB 1.1) host controller**. The host's `uhci_hcd` binds it and enumerates
three always-or-dynamically-present devices:

| Device | Class | Source |
|--------|-------|--------|
| **Keyboard** | HID | vKVM/IRC keystrokes (`hid`, `hid_codes`) — always connected |
| **Mouse** | HID | vKVM/IRC pointer (persistent-mouse option) — always connected |
| **Mass storage** | BOT / SCSI | virtual **floppy** (`.img`), **CD/DVD** (`.iso`), USB key, virtual folder — attaches dynamically |

## Emulation model — software UHCI

UHCI is **emulated in BMC software**, not a hardware USB controller:

- Kernel BSP IODevices: **`IODUHCIHC`** (index `0x33`, the host-controller face),
  **`IODUHCIWALKER`** (index `0x34`, the schedule walker), **`IODUSB`** (index `0x16`). IRQ
  51/52 (docs/12). The HC register base lives in the BSP IODevice config table (keyed by index).
- Host-facing UHCI I/O registers (USBCMD, USBSTS, USBINTR, FRNUM, FLBASEADD, SOF, PORTSC) are
  presented through PCI `.4` I/O BAR `0x3c00` (docs/06). Writes are trapped by the endpoint.
- **The walker** (`uhci_hcq`, `UHCI_WALKER_CMD`, `UHCI_UHCWSTAT`) reads the host's **frame-list +
  TD/QH schedule from host memory** (over the shared-memory window), executes each USB transfer
  against the virtual device backend, and writes TD status back. `uhci_stm_interrupt` drives the
  controller state machine; `PORTSC`/`uhci_connect` emulate device attach/detach;
  `uhci_wait_for_pwr_event` ties into host-power (devices appear when the host powers on).

## Virtual-device backend (`usbservice_c.c`)

Serves the USB transfers the walker dispatches:
- **HID endpoints** (interrupt-IN): keyboard + mouse reports, fed from the vKVM/IRC input stream.
- **Mass storage** (bulk-only transport): answers SCSI (`INQUIRY`, `READ CAPACITY`, `READ`/
  `WRITE`) against the mounted image.

## Virtual-media manager (`vm_*`)

Backs the mass-storage device with a media source:
- **`vm_applet`** — interactive media via the IRC Java/ActiveX applet (local drive/image).
- **`vm_script` / `vm_url`** — scripted media: an HTTP(S) **URL** to a `.img`/`.iso`
  (`vm_url_connected`, `image_url`, `URL_REQUIRED`). 1.44 MB floppy or CD/DVD ISO.
- **`vm_stream`** — the data stream for bulk reads. States: `vm_connected`/`vm_status`;
  `SCSI_ERR_APPLET_CONNECT` on failure. Devices: `FLOPPY`, `CDROM`, USB key, virtual folder;
  commands `INSERT`/`EJECT`.

## Data flow (one block read, host → media)

1. Host OS reads a block → `uhci_hcd` queues a BOT CBW in a TD on the UHCI frame list (host RAM).
2. BMC walker reads the TD over the shared window, hands the SCSI `READ(10)` to `usbservice`.
3. `usbservice` → `vm_*` fetches the blocks from the source (HTTP URL / applet / stream).
4. Data is returned through the bulk-IN TDs (written to host RAM over the window); TD status set;
   walker raises the UHCI completion interrupt to the host.

## OpenBMC port — the hard part

No OpenBMC equivalent exists. A port needs, in order of difficulty:
1. **UHCI device-controller emulation** on the Gromit-XE PCI `.4` endpoint — trap host UHCI
   register access + walk the host frame-list/TD schedule over the CHIF shared window. Net-new;
   nothing in-tree emulates UHCI toward a host.
2. **Virtual USB devices** — HID keyboard/mouse + USB mass storage (BOT/SCSI). Could reuse Linux
   **USB gadget** function logic (`f_hid`, `f_mass_storage`) IF the emulated controller exposes a
   UDC-like interface; otherwise bespoke.
3. **vMedia backend** — mount an `.img`/`.iso` from a URL/stream behind the mass-storage device.
   OpenBMC's modern answer is **nbd-proxy / virtual-media (bmcweb) + a block backend**; the HID
   input side pairs with **obmc-ikvm** (which already does KVM video, docs/14).

This depends on the CHIF server (docs/28) for the shared-memory transport and on host-power
(devices attach on power-on). It is the gating work for a *functional* vKVM/vMedia port and is
realistically a multi-component effort, not a single driver.

## Open

- UHCI HC register base (in the BSP IODevice config table, keyed by index 0x33) — not a code
  literal; recover from the `.int` config or on hardware.
- Exact TD/QH walk + the shared-window addressing the walker uses for host-memory DMA.
- HID report formats + the SCSI command subset implemented.
