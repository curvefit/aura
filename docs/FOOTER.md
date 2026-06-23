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
```

`CompiledAuraPlan` is built from `AURP` once per file in profiled direct paths
and replay benchmarks. Its `conversion_plan_hash` hashes the encoded footer
bytes and identifies the conversion metadata used for a benchmark run.

Invalid magic, unsupported version, invalid field counts, invalid field indexes,
wide i64-incompatible schema fields, and header/schema disagreement reject before
hot-loop decoding.

## Missing Metadata For The Zstd-Speed Byte Target

The current compiled footer describes semantic decode/reconstruct programs. It
does not contain an Aura1 byte lane: compressed Aura1-compatible body slices
with per-block offsets and validation metadata. That is why current Aura0 byte
expansion must decode streams and rebuild rows, while `.aura1.zst` can inflate
already-formed Aura1 bytes.

A future byte-lane footer extension would need at least:

```text
byte-lane codec and level
row range per compressed block
uncompressed Aura1 offset and length
compressed body offset and length
per-block checksum or output-byte guard
optional codec dictionary id
compatibility flag: acceleration cache vs authoritative payload
```

Until those fields exist and are tested, AURA0 should not claim zstd-beating
decode-to-Aura1 byte speed for the fair product target.
