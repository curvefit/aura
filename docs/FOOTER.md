# AURA Footer

Footers are immutable conversion metadata. They are discovered from EOF:

```text
... body bytes ...
footer bytes
u32 footer length
"sealed:)"
```

The current implementation has two footer formats:

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
