# Conversion flow

`.aura` ingest files are the easiest canonical source because they keep the
logical stream plus seal-time stats. `.aura0` and `.aura1` are compiled from the
same logical stream into code-only field programs.

```text
.aura  -> .aura0
.aura  -> .aura1
.aura0 -> .aura1
.aura1 -> .aura0
```

The conversion is intentionally simple when every level preserves the same
logical facts:

```text
decode chunk
materialize logical records in file order
apply the target field program
write compact deltas or replay blocks
```

This is easy to parallelize across chunks. Conversion should not require network
access, text parsing, decimal parsing, or source-specific logic. Schema-specific
logic belongs in the Aura schema definition, not in collectors or converters.
When converting from a compiled file back to `.aura`, the converter can decode
the field program, materialize generous logical records, and recompute any
missing ingest footer stats while it writes the derived `.aura` file.

The current crate provides small in-memory helpers for generic integer records
and OHLCV Parquet input. The production version should stream chunk-by-chunk and
write temporary output files that are atomically promoted after validation.
