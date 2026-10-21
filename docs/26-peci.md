# iLO 3 PECI engine — CPU temperature / config transport

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`src\peci.c`,
`ARM:LE:32:v5`, base 0; code addr = file offset). Register map + init CONFIRMED by
disassembly. **Corrects docs/09/25**: the native PECI engine is at **`0x01F00400`**, not
CPLD `0x01F14068` (that is only the multi-node target MUX).

## Engine register block — base `0x01F00400` (CONFIRMED)

| Off | Reg | Role | Evidence |
|-----|-----|------|----------|
| `+0x00` | **CMD** | write a command-descriptor (RAM buffer pointer) = start the transaction | `str r2,[=0x1f00400]` @ `0x11cb6c` |
| `+0x04` | **STATUS** | `(val & ~1) != 0` ⇒ bus error | `ldr; bics ip,r2,#1; bne err` @ `0x11cbc8` |
| `+0x08` | **TIMING** | `PECI_BIT_TIMING_CONFIGURATION` | `str r0,[=0x1f00408]` @ `0x11c4b0` |
| `+0x0C` | **ENABLE** | write `0x80` to enable | `mov r0,#0x80; str [r1,#4]` @ `0x11c4b8` |

- **Completion**: IRQ **60** (`IODPECI`, docs/12); the IODevice wait returns code **`0xD1`** on
  timeout (`cmp r0,#0xd1` @ `0x11cb84`, logs `PECI ERR TIMEOUT`).
- **Bit-timing** picked from the **rev strap `0x01F100A0` bits[23:16]** (`0x11c494`): `<= 0x04`
  ⇒ **`0x3D090010`** (slow), else **`0x61A80019`** (fast).
- **Multi-node target MUX** (XNC/multi-socket only): CPLD **`0x01F14068`** — bits[5:0] target,
  [7:6] preserved (`0x11cc6c`, `XNC NOT ready - failed PECI MUX target`). Single-socket G7 leaves
  it at default.

## Transaction model (CONFIRMED engine / descriptor best-effort)

`run_peci_cmd` (`0x11c7a4`) builds a ~`0x20`-byte command descriptor `{flag=1, cmd, params…}`
(`strb` into `[sp+0x34..0x38]`), the low-level xfer (`0x11cb68`) writes its pointer to CMD,
waits on the IODPECI IODevice (IRQ 60), checks STATUS, then copies the response bytes out of the
buffer. Because device regions are identity-mapped (VA=PA, docs/10) a plain RAM pointer works as
the engine's DMA source/sink.

**CONFIRM-ON-HARDWARE**: the exact descriptor field order and the response-buffer offset are only
partially recovered (the fw marshals via the IODevice abstraction). The four engine registers,
timing values, `0x80` enable, IRQ 60, `0xD1` timeout and the STATUS error test are CONFIRMED.

## Two PECI paths (don't conflate)

- **Native PECI** (this doc): CPU DTS temperature, `RdPkgConfig`, `peci_read_pciconfig` — engine
  `0x01F00400`, IRQ 60.
- **PECI-over-SMBus** (`peci_read_one_DIMM_register`, `peci_read_one_MB_temperature`,
  `peci_read_byte`): DIMM SPD + motherboard temps via an **SMBus** controller behind a PCA9542
  mux (`SMB command to select 9542 Mux channel`) — a different transport (I2C), not this engine.

## Driver + port

`rebuild/meta-ilo3/recipes-kernel/linux/files/peci-gxe.c` = a Linux **PECI controller**
(`hpe,gxe-peci`, `reg = <0x1f00400 0x10>`, `interrupts = <60>`, `hpe,cpld = <&cpld>`): the kernel
PECI core issues standard commands (Ping/GetDIB/GetTemp/RdPkgConfig/RdPCIConfig) through
`->xfer`; the driver does the init (strap→timing→enable) and runs each command via the
descriptor-DMA model above. `dbus-sensors` `peci-cputemp` then publishes CPU temps to D-Bus/Redfish.
