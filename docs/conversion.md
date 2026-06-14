# Conversion flow

`.aura` ingest files are the canonical compile source because they keep the
logical stream plus seal-time stats and stamped physical plans. `.aura0` and
`.aura1` are compiled from the same stamped `.aura` into code-only field
programs.

```text
.aura  -> .aura0
.aura  -> .aura1
.aura0 -> .aura1
.aura1 -> .aura0
```

The conversion is intentionally simple because the compiled footer carries both
profile programs and is copied unchanged between compiled profiles:

```text
read stamped footer
decode logical records in file order
apply the target field program
write compact deltas or replay blocks
copy the same footer
```

This is easy to parallelize across chunks. Conversion should not require network
access, text parsing, decimal parsing, or source-specific logic. Schema-specific
logic belongs in the source adapter's Aura schema definition, not in compiled
profile converters.

Compiled files are not optimization sources: conversion must not re-score or
mutate the footer. A reader can materialize logical records from `.aura0` or
`.aura1`, then write the other compiled body using the other program already in
the same footer.

The current crate provides small in-memory helpers for generic integer records
and OHLCV Parquet input. The production version should stream chunk-by-chunk and
write temporary output files that are atomically promoted after validation.
