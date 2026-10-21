# iLO 3 boot ROM — signature verification (secure boot)

Date: 2026-10-20. Static RE of the boot ROM (`decompiled/bootrom_7f0000.bin`, raw
`ARM:LE:32:v5` @ `0x7f0000`). How the boot ROM verifies the firmware image before booting —
the gating fact for whether alternative (OpenBMC) firmware can be flashed.

## Scheme: RSA signature over a SHA-512 hash

| Element | Finding | Evidence |
|---------|---------|----------|
| **Hash** | **SHA-512** | IV table `0x7f4228` (H0 `0x6a09e667`, H1 `…84caa73b`, …) + K table `0x7f4268` = SHA-512 constants as 64-bit pairs (K0 `0x428a2f98d728ae22`, K1 `0x7137449123ef65cd`, K2 `0xb5c0fbcfec4d3b2f`, K3 `0xe9b5dba58189dbbc`) |
| **Signature** | **RSA** public-key verify ("decrypt" the signature → compare to the hash) | `Starting decrypt`, `Starting hash`, `Signature valid` |
| **Public key** | in the **embedded certificate** (DER/PEM @ flash `0x7e0000`), chained to an HP **root of trust** — NOT a plain modulus in the boot ROM (entropy scan found no embedded key) | cert block, docs/01/03 |
| **Signature block** | flash `0x800000` (image signature) | docs/01/03 |
| **Decision** | hash image → RSA-verify vs the cert key → `Signature valid` boots; otherwise `unsigned - will not boot an image` | `0x7f015c`/`0x7f0168` |

## Boot flow (docs/03 + this pass)

`locate image → Starting hash (SHA-512) → Starting decrypt (RSA verify signature) →
Signature valid → Starting decrypt (image, if encrypted) → Decompressing (LZSS) → Copying image
→ boot`. The loader recognises magics `iLO3iLO4iLO5iLO6iLO7` — the **same verified-boot loader
spans iLO 3–7**.

## NVROVR — NOT a signature bypass

`NVROVR - starting countdown` / `NVROVR - countdown complete, resuming` is a boot-time **NVRAM
override countdown** — a recovery window (e.g. to escape a bad config / force a setting at boot),
tied to the `SCBNON/SCBCLD/SCBWRM` boot-state tags. It does **not** disable signature checking.

## Implications for flashing OpenBMC (the project goal)

The boot ROM is a **verified-boot root**: it rejects any image not RSA-signed under HP's cert
chain. Consequences:

1. **Can't just flash unsigned firmware** through the normal path — `unsigned - will not boot`.
   HP's private signing key is not available, so OpenBMC images can't be legitimately signed.
2. **The boot ROM lives in the external SPI flash** (flash `0x7f0000`, same chip as the image,
   docs/03) — so it is itself rewritable at the hardware (SPI programmer) level. Whether that
   enables unsigned boot depends on **what verifies the boot ROM**:
   - If an **on-die mask ROM** first-stage verifies the flash boot ROM (a hardware root of
     trust), then patching the flash boot ROM's signature check is defeated by the mask ROM →
     unsigned boot is blocked without breaking the chain at silicon level.
   - If there is **no on-die verification** of the flash-resident boot ROM, then a full-flash
     rewrite (external SPI programmer) that patches the `Signature valid` decision OR replaces the
     boot ROM entirely could run unsigned firmware. **This is the open question** (docs/03:
     boot-ROM load/run address + what runs before `0x7f0000` is unconfirmed).
3. **Legitimate path**: none without HP's key. **Research/repair path**: determine the silicon
   root of trust (is there a mask ROM above `0x7f0000`?) — trace the reset vector / earliest
   execution. If the flash boot ROM is the top of the chain, a patched/replaced boot ROM via SPI
   is the owner's route to running OpenBMC on hardware they own.

## Root of trust — RESOLVED: flash boot ROM is the reset target

Traced the ARM reset vector (`docs/03` RE pass 2):

- The boot ROM **starts with the ARM exception vector table** (`0x7f0000`: `b reset` → `0x7f002c`,
  undef/swi/pabt/dabt/irq/fiq). Its literal pool self-references **`0xFFFF0xxx`** (`0xFFFF0e7c`,
  `0xFFFF02a8`), and the reset handler's `bl` targets all resolve inside the ROM at base
  **`0xFFFF0000`** — so the boot ROM's true runtime base is **`0xFFFF0000`** (ARM926 **high
  vectors**, VINITHI strap = 1), not `0x7f0000`.
- The **8 MB SPI flash is mapped at `0xFF800000`–`0xFFFFFFFF`**, so flash offset `0x7f0000` lands
  exactly at `0xFFFF0000`. The ARM926 **resets straight into the external SPI flash** and runs the
  reset handler at `0xFFFF002c` (a full cold init: CP15, stack `0x20004000`, clock/DDR/peripheral
  bring-up, then image verify + boot).

**Conclusion: the flash-resident boot ROM is the top of the trust chain.** There is no evidence of
an on-die mask ROM between reset and the flash code — the reset vector *and* the complete
cold-reset first-stage live in rewritable flash. This is consistent with **iLO 3 (Gen7, ~2010)
predating HPE's "Silicon Root of Trust"**, which HPE introduced as a *new* Gen10/iLO 5 feature
(~2017); iLO 3/4 have no immutable hardware anchor.

### Reflash verdict (the project goal): FEASIBLE via external SPI programmer

Because the signature-check code (`Signature valid` / `unsigned - will not boot`) lives in the
**rewritable** flash boot ROM and nothing in silicon verifies that boot ROM, an owner with an SPI
flash programmer can:
- **patch out** the signature-check branch in the boot ROM, or
- **replace** the boot ROM with an unsigned first-stage (e.g. a Gromit-XE U-Boot SPL using the
  DDR/clock init in `docs/30`),
and boot **unsigned OpenBMC** on hardware they own. The RSA/SHA-512 enforcement is a firmware
policy, not a hardware root of trust.

**Caveat (honesty):** this is inferred from (1) the reset vector + full cold-init handler residing
in flash, (2) the self-referencing `0xFFFF0000` literals, and (3) iLO 3 predating HPE SRoT.
100% certainty needs a hardware test — flash a boot ROM with the check patched and confirm it
boots (i.e. that no undisclosed mask ROM re-verifies it). All available evidence says it will.

## The signature-check branch — LOCATED (the patch point)

The verify function is at runtime `0xFFFF3800` (boot-ROM file `0x3800`). It:

1. `0xFFFF3860` **SHA-512 init** (`bl 0xFFFF2ca8`, ctx on stack).
2. `0xFFFF3870` + `0xFFFF388c` **SHA-512 update ×2** (`bl 0xFFFF32ac`) — hashes the image around
   the signature region (two ranges, skipping the embedded sig).
3. `0xFFFF3898` **SHA-512 finalize** (`bl 0xFFFF358c`) → 64-byte digest to buffer `0xA1001EC4`.
4. `0xFFFF38b4` **compare 0x40 (64) bytes** (`bl 0xFFFF364c`): computed digest (`0xA1001EC4`) vs
   the **RSA-decrypted expected hash** (`r7`, from the earlier "Starting decrypt" RSA op on the
   signature using the cert key). 64 bytes = SHA-512 digest length.
5. **`0xFFFF38b8`: `cmp r0, #0`** — compare result (0 = digests equal).
6. **`0xFFFF38bc`: `bne 0xFFFF39c4`  (bytes `1a 00 00 40`)** — **THE decision**. Equal → fall through
   to `0xFFFF38c0` "Signature valid" → boot. Not-equal → `0xFFFF39c4` returns failure → caller
   halts with `unsigned - will not boot an image`.

**Location in the flash image**: boot ROM is at flash `0x7f0000`, so the branch is at
**`ilo3_194.bin` offset `0x7f38bc`** (verified: the 4 bytes there are `40 00 00 1a` = `bne`,
preceded by `cmp r0,#0` = `00 00 50 e3`, followed by the "Signature valid" ADR `56 0f 8f e2`).

### Minimal patch for unsigned boot (owned hardware)

Replace the branch at flash `0x7f38bc` so it never diverts to the failure path:

```
offset 0x7f38bc:  1A 00 00 40  (bne 0xFFFF39c4)   ->   00 00 A0 E1  (nop / mov r0,r0)
```

With the `bne` NOP'd, the compare result is ignored and execution always falls through to
"Signature valid" → any image (unsigned OpenBMC) boots. Re-flash the patched image with an
external SPI programmer. (Equivalent alternatives: NOP the `cmp` at `0x7f38b8`, or force `r0=0`
before it.) Since nothing verifies the boot ROM itself (docs/31 above), this patch is not
re-checked.

> Scope: this is for running alternative firmware on **hardware the owner possesses** (the project
> goal — OpenBMC on EOL ProLiant G7), per the repo legal note. It is a single-instruction edit to
> the device's own rewritable boot loader, not an attack on a third party.

## Open

- **Hardware confirmation**: flash the NOP'd boot ROM and verify an unsigned image boots (proves
  no undisclosed mask ROM re-verifies the boot ROM — all static evidence says none does).
- RSA key size + cert chain depth (parse the `0x7e0000` cert block) — not needed for the patch,
  only to understand the signing scheme.

- Locate the exact `Signature valid` branch (the one-instruction patch point) — the logger is
  tag-based; find it via the RSA-compare result. Then the minimal reflash patch is known.
- RSA key size + cert chain depth (parse the `0x7e0000` cert block).
- Hardware confirmation that a patched boot ROM boots (no silicon re-verify).

- **The silicon root of trust**: does an on-die mask ROM verify the flash boot ROM at `0x7f0000`?
  Trace the ARM reset vector and the earliest `0xC0000000`/flash access — the single most
  important question for the reflash goal.
- RSA key size (2048 vs 4096) and the cert chain depth — parse the `0x7e0000` cert block.
- Exact location of the `Signature valid` branch (the patch point if the flash boot ROM is the
  root) — the strings use a tag-based logger not trivially xref'd; locate via the RSA-compare.
