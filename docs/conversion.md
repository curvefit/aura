# Conversion flow

`.aura` ingest files are canonical. `.aura0` and `.aura1` are compiled from the
logical stream plus the seal-time stats stored in the ingest footer.

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
apply the target physical plan
write compact deltas or replay blocks
```

This is easy to parallelize across chunks. Conversion should not require network
access, text parsing, decimal parsing, or source-specific logic. Schema-specific
logic belongs in the Aura schema definition, not in collectors or converters.

The current crate provides small in-memory helpers for the older book prototype.
The production version should stream chunk-by-chunk and write temporary output
files that are atomically promoted after validation.
