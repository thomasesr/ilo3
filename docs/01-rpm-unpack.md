# 01 — RPM unpack & flash image identification

## Source
- `firmware/firmware-ilo3-1.94-1.1.i386.rpm` (SHA256 verified OK on download)
- Extracted with `rpm2cpio ... | cpio -idmv` → `extracted/rpm/`

## RPM payload tree
```
usr/lib/i386-linux-gnu/firmware-ilo3-1.94-1.1/
  ilo3_194.bin      8,400,072 B   <-- FLASH IMAGE (target)
  CP046328.xml        246,900 B   HP CPQ package descriptor
  readme.txt            4,949 B   version + supported servers
  .cpq_package.inc / .setup / setup / hpsetup   installer scaffolding
usr/lib/i386-linux-gnu/{scexe-compat,hp-scexe-compat}/CP046328.scexe  (same payload, scexe form)
```

## Product facts (from readme.txt / .cpq_package.inc)
- Product: **HP Integrated Lights-Out 3 (iLO 3)** firmware **1.94** (build 1.94.2, 06-Dec-2020)
- Target hardware: **HP ProLiant G7** servers (DL360/DL380/DL385/DL580/DL585 G7, BL*c G7, ML/SL G7, etc.)
- SWKEY `firmware:ilo3`, component `CP046328`

## Flash image: `ilo3_194.bin`
- Working copy: `firmware/staging/ilo3_194.bin`
  SHA256 `d84774acf71d67f0aeae446d76195e5ab084379f3463e5a9b1a0de6a6f754903`
- `file` → `HPE iLO3 firmware update image, version 1.94.2 06-Dec-2020` (known magic)
- Size 0x802cc8 (8,400,072 B)

### Header (offsets byte-accurate from `od`)
```
0x00  "iLO3 v 1.94.2 06-Dec-2020\x1a"   version magic, 26 bytes
0x1a..0x24  00 + 0xFF padding
0x24  u32 0x00001156
0x28  u32 0x00104bca   (~1.06 MB — section size/bound, TBC)
0x2c  u32 0x007e0000   == tail/cert region start (matches entropy map)
0x30  u32 0x00800000   == signature block @ 8 MB (matches entropy map)
0x34  0xFFFFFFFF  0x38 0x00000000  0x3c 0xFFFFFFFF
0x40  payload begins (immediately high-entropy)
```

### Entropy map (64 KB blocks, normalized 0..1)
```
0x000000–0x660000  ~0.91 (0.95 @ 0x2d0000–0x4c0000)  main payload (compressed/encrypted)
0x670000–0x6e0000   0.000                             zero padding (~512 KB erased gap)
0x6f0000–0x750000  ~0.90                              payload section 2
0x770000–0x7d0000  ~0.90                              payload section 3
0x7e0000–EOF        0.3–0.75                          cert/signature tail
```

### binwalk signature hits (real vs noise)
- Real: DER x509 cert @0x2D2D0F; multiple PEM certs @0x80020F..0x802792 (signature/cert tail).
- Real-ish: QNX4 Boot Block magic @0x72C3D1 and @0x7AC3D1 — investigate (could be real FS or coincidental).
- Noise: "Zip uncompressed 1.8 GB", "PGP RSA session key", "HTML document" — false hits inside high-entropy region.

## Open questions → next step
1. **Payload nature: compressed or encrypted?** Entropy ~0.91 (below ~0.99 AES) leans *compressed/obfuscated*.
   - Probe: byte histogram of a payload block (uniform=encrypted, skewed=compressed);
     test for short XOR key / repeating pattern.
2. Decode remaining header fields → full section/partition table (offset, size, type per section).
3. Investigate QNX4 matches @0x72C3D1 / 0x7AC3D1.
4. Leverage public research: Synacktiv **ilo4_toolbox** (iLO4 .bin format parser) + iLO3 notes —
   iLO3/4 share HP ROM image lineage; their header/section layout is documented.

## Tool note
- `binwalk -E` (entropy) crashes in this install (numba/numpy bug). Use the stdlib
  entropy script instead (inlined in session; recreate under `tools/` if reused).
