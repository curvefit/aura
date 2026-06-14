# Conversion flow

`.aura` ingest files are the canonical compile source because they keep the
logical stream plus seal-time stats and stamped physical plans. `.aura0` and
`.aura1` are compiled from the same stamped `.aura` into code-only field
programs.

```text
.aura  -> .aura0
.aura  -> .aura1
```

The conversion is intentionally simple because the target field program has
already been chosen while writing the stamped `.aura`:

```text
read stamped .aura footer
read logical records in file order
apply the target field program
write compact deltas or replay blocks
```

This is easy to parallelize across chunks. Conversion should not require network
access, text parsing, decimal parsing, or source-specific logic. Schema-specific
logic belongs in the source adapter's Aura schema definition, not in compiled
profile converters.

Compiled files are decode artifacts, not optimization sources. A reader can
materialize logical records from `.aura0` or `.aura1`, but producing another
optimized profile should go back to the stamped `.aura` source or write a new
`.aura` and stamp it explicitly.

The current crate provides small in-memory helpers for generic integer records
and OHLCV Parquet input. The production version should stream chunk-by-chunk and
write temporary output files that are atomically promoted after validation.
