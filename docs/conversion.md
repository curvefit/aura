# Conversion flow

Cold is canonical. Other profiles are derived.

```text
cold -> warm
cold -> ultra hot
warm -> ultra hot
```

The conversion is intentionally simple when cold stores scaled integer facts:

```text
decode chunk
resolve timestamp and sequence deltas
resolve price deltas
copy absolute quantities
write fixed-width hot events
```

This is easy to parallelize across chunks. Conversion should not require network
access, text parsing, decimal parsing, or source-specific logic.

The current crate provides small in-memory helpers for these transitions. The
production version should stream chunk-by-chunk and write temporary output files
that are atomically promoted after validation.
