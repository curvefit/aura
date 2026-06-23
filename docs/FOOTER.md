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
