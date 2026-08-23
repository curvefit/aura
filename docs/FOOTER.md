# AURA Footer

Footers are immutable conversion metadata. They are discovered from EOF:

```text
... body bytes ...
footer bytes
u32 footer length
"sealed:)"
```

The current implementation has two footer magic families:

- `AURF`: ingest footer for `.aura`
- `AURP`: compiled footer for `.aura0` and `.aura1`

## AURF

The ingest footer stores schema, observed statistics, optional compiled plans,
and chunk descriptors. It preserves the facts required to compile `.aura` into
`.aura0` or `.aura1`.

## AURP

The compiled footer stores the shared metadata for interchange between `.aura0`
and `.aura1`:

```text
magic AURP
format version
compression descriptor
record count
Aura1 block capacity
schema
Aura0 decode program
Aura1 decode program
generic Aura0 instruction plan
chunk descriptors
optional Aura1 byte-lane descriptor table
```

`CompiledAuraPlan` is built from `AURP` once per file in profiled direct paths
and replay benchmarks. Its `conversion_plan_hash` hashes the encoded footer
bytes and identifies the conversion metadata used for a benchmark run.

Invalid magic, unsupported version, invalid field counts, invalid field indexes,
wide i64-incompatible schema fields, and header/schema disagreement reject before
hot-loop decoding.

## Aura0-Fast Byte Lane

The compiled footer can end with an optional `AUBL` extension. Empty compact
files omit the extension entirely, so current compact files keep their existing
footer shape. New readers decode old compact files normally.

Current descriptor layout:

```text
magic "AUBL"
descriptor count u32
for each descriptor:
  lane_version u8
  codec_id u8          # raw=0, lz4=1, zstd=2
  codec_level u8       # raw/lz4=0, zstd=1|3|9
  checksum_kind u8     # none=0, output byte guard=1
  block_index u32
  row_start u64
  row_count u32
  aura1_output_offset u64
  uncompressed_len u64
  compressed_offset u64
  compressed_len u64
  checksum u64
  flags u32
```

The first production implementation writes a single descriptor for full Aura1
file bytes. `aura0-fast` stores only this byte lane. `aura0-hybrid` stores the
semantic stream lane first and appends the byte-lane payload; descriptor
`compressed_offset` points at the appended payload.

Reader selection:

```text
auto:   use byte lane when present, otherwise semantic lane
always: require byte lane or reject clearly
never:  force semantic lane
```

Production timing validates descriptor structure and lengths but does not scan
the output guard. Verify/strict mode validates the output-byte guard and output
equality when a reference is supplied.

Compatibility caveat: old readers that require `AURP` to end immediately after
chunk descriptors will reject new byte-lane footer extensions as trailing bytes.
This is an intentional forward-versioning boundary for the fast/hybrid profiles;
new readers still read old compact files.

## AURP V3 flat Aura0 footer V1

The decode-first V3 Aura0 subset reuses `AURP` but dispatches on container
version 3 and layout version 1. Its fixed prefix is exactly 176 bytes, all
little-endian:

| Offset | Width | Meaning |
| ---: | ---: | --- |
| 0 | 4 | `AURP` |
| 4 | 2 | container version 3 |
| 6 | 2 | flat layout version 1 |
| 8 | 1 | body encoding 1, concatenated exact-value blocks |
| 9 | 3 | compression kind, level, flags; all zero |
| 12 | 8 | record count |
| 20 | 8 | body length |
| 28 | 4 | column count |
| 32 | 4 | chunk count |
| 36 | 4 | schema ID routing hint |
| 40 | 2 | primary timestamp slot zero, or `ffff` when absent |
| 42 | 2 | primary U64 sequence slot, or `ffff` |
| 44 | 4 | reserved, zero |
| 48 | 32 | canonical schema fingerprint |
| 80 | 32 | domain-separated exact-header SHA-256 |
| 112 | 32 | domain-separated exact-body SHA-256 |
| 144 | 32 | global canonical logical-row SHA-256 |

The prefix is followed by the exact length-prefixed tag-4 schema descriptor.
The stats table begins with version u16=1, descriptor size u16=36, and count
u32. Each descriptor contains slot u16, type u8, flags u8, present count u64,
null count u64, logical payload bytes u64, maximum present value byte length
u32, and reserved u32=0. Flag bits are nullable, variable-width, and fixed
numeric/timestamp. Variable logical payload counts a u32 length plus raw bytes
for each present value; its maximum excludes the u32 prefix.
Footer-only decode validates fixed payload totals and widths exactly, bounds
variable totals by the present count and maximum value length, and applies the
effective per-value limit before any body is available.

The chunk table begins with version u16=1, descriptor size u16=136, and count
u32. Each descriptor contains chunk ID u32, flags u32, first global row u64,
row count u32, exact-block version u16=1, reserved u16=0, body-relative offset
u64, stored length u64, plain stored SHA-256, canonical chunk logical SHA-256,
first/last primary timestamps as i64, and first/last primary sequences as u64.
Flag bits declare timestamp and sequence bounds. Bounds are the first and last
present values in row order; absent flags require zero pairs.

The final 32 bytes are SHA-256 over
`aura-v3-flat-aura0-footer-v1\0`, the pre-hash footer length as u64 LE, and all
preceding footer bytes. Header and body hashes use the analogous `header-v1`
and `body-v1` domains, a u64 exact length, and exact bytes. Stored chunk hashes
are plain SHA-256. Complete decode verifies all four hash layers, schema/header
agreement, exact table exhaustion, contiguous row/body ranges, per-field stats,
and timestamp/sequence bounds.
