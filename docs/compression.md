# Compression policy

Aura should prefer independently compressed chunks over whole-file compression.

Aura0 profile:

```text
chunked zstd
high compression level
chunk directory required
chunk-local decode state
```

Aura1 profile:

```text
uncompressed for maximum replay speed
or chunked low-level zstd when disk matters
```

Whole-file compression can produce slightly smaller files, but it makes parallel
conversion, partial validation, and corruption isolation worse. `.aura0` should
preserve enough per-chunk metadata to let workers convert chunks to `.aura1`
files independently.
