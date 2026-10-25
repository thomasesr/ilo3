# ilo3 — clean-room Rust firmware for the HP iLO 3 (Gromit-XE) BMC

A modern, **clean-room** replacement firmware for **HP iLO 3** (Integrated Lights-Out 3), the
end-of-life BMC on HP ProLiant **Gen7** servers (target: **DL380p G7**). Written from scratch in
`no_std` **Rust** — not an OpenBMC port — to fit the real hardware (250 MHz ARMv5, ~128 MB LPDDR2,
8 MB flash) and bring current crypto/TLS to hardware that modern browsers and SSH clients have
otherwise abandoned.

> **Status: software-complete across all phases; hardware validation pending.**
> Everything buildable without the physical board is written and compiles clean for the
> `armv5te-ilo3` target. Real-hardware bring-up (via an SPI flash programmer) is the remaining
> work — see **[`docs/48`](docs/48-hw-bringup-runbook.md)**.

## Why

iLO 3 is EOL: no firmware updates, no security patches, weak legacy crypto (SSH-1/3DES, old TLS).
On hardware people still run, recovering a buildable, patchable firmware keeps it secure and alive.
This project reverse-engineers the iLO 3 firmware to register-exact hardware maps + service
contracts, then **reimplements** the stack clean: hardware register maps are facts; standard
protocols (HTTP/TLS, SSH, SNMP, IPMI, SNTP, DHCP) are built from RFCs + Rust crates; HP-proprietary
wire formats (RIBCL, CHIF, DVC) are reimplemented from RE'd behaviour — never by copying HP code.

## Hardware (confirmed)

- **SoC**: HP "Gromit-XE" ASIC, **ARM926EJ-S** (ARMv5TE), resets into SPI flash at `0xFFFF0000`.
- **Boot flash**: Macronix **MX25L6455E** — 8 MB SPI NOR.
- **SRAM**: ISSI IS62WV2568BLL — pre-DDR scratch at `0x20000000`.
- **DRAM**: ISSI IS43/46LD16640A — **1 Gbit LPDDR2-S4** (128 MB), RL7/WL4 (~400–466 MHz).

## What's built

Firmware crate `rebuild/ilo3-rs/` (+ a separate `spl/` first-stage loader):

- **Bring-up**: reset/CP15, MMU (identity + high-vector alias), 2-bank irqchip, timer + live IRQ,
  `embassy` async executor on ARMv5, `embassy-time` driver; **SPL** (DDR/clock init → load → jump),
  `tools/pack-flash.py` builds the 8 MB image.
- **HAL**: CPLD-gpio, SPI-NOR, I2C (8 engines), PECI, fan scan-chain, MAC + MDIO.
- **Services**: host power, sensors, the **safety-critical thermal/fan controller** (fail-safe:
  boot=MAX, sensor-fault→MAX, critical→shutdown), PSU (panda) telemetry, drive backplane (SEP).
- **Networking**: `smoltcp` TCP/IP + DHCP (datapath gated on the MAC DMA-ring reconstruction).
- **Crypto/TLS**: software entropy pool + ChaCha20 CSPRNG, TLS 1.3 primitives (AEAD/SHA-256/HKDF/
  X25519) + power-on self-test, key schedule + record layer, flash cert/key store, 15 MB heap.
- **Management**: HTTPS web UI (picoserve) with session auth, SSH CLI (SMASH CLP), SNMP v1/v2c,
  IPMI 2.0 + KCS, RIBCL (XML), coalescing IML/iLO event log + EventBus.
- **Host interface**: CHIF transport + routing, SMBIOS inventory.
- **Remote console**: RFB/VNC KVM (noVNC), vUSB/vMedia (UHCI + USB-MSC/HID), VSP — authenticated.

See **[`PLAN-rust.md`](PLAN-rust.md)** for the phase-by-phase roadmap + status, and
**[`docs/`](docs/)** for the reverse-engineering record (register maps, init sequences, protocol
decodes) that the rewrite is built from.

## Build

Requires the `nightly` toolchain with `rust-src` + `llvm-tools-preview` (tier-3 target →
`-Zbuild-std`). See `rebuild/ilo3-rs/README.md` for details.

```sh
cd rebuild/ilo3-rs
cargo +nightly build --release          # firmware + spl crates
# flat images + 8 MB flash image:
llvm-objcopy -O binary target/armv5te-ilo3/release/ilo3-fw  ilo3-fw.bin
llvm-objcopy -O binary target/armv5te-ilo3/release/ilo3-spl ilo3-spl.bin
python3 tools/pack-flash.py ilo3-fw.bin ilo3-spl.bin flash.bin
```

Flashing + first boot on real hardware: **[`docs/48-hw-bringup-runbook.md`](docs/48-hw-bringup-runbook.md)**.

## Layout

```
rebuild/ilo3-rs/   the Rust firmware (firmware/ + spl/ crates, tools/)
docs/              reverse-engineering findings (NN-topic.md); the clean-room source
PLAN-rust.md       the rewrite roadmap + RE coverage audit
tools/             image packaging + analysis scripts
```

Not in this repo (you supply your own, for hardware you own): HP's copyrighted firmware images and
any binaries decompiled from them (`firmware/`, `decompiled/`, `extracted/` are git-ignored). The
`docs/` reference offsets into those binaries; obtain the matching iLO 3 FW 1.94 image from HP.

## Legal

Reverse engineering for **interoperability, repair, and continued operation of hardware the owner
possesses**. This repo contains only original clean-room code + RE notes; it does **not**
redistribute HP's copyrighted firmware binaries. HP, iLO, and ProLiant are trademarks of
Hewlett Packard Enterprise; this project is not affiliated with or endorsed by HPE.

## License

[GNU GPL v3.0](LICENSE) © Thomas Richter.
