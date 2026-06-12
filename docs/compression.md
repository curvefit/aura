# Compression policy

Aura should prefer independently compressed chunks over whole-file compression.

Cold profile:

```text
chunked zstd
high compression level
chunk directory required
chunk-local decode state
```

Hot profiles:

```text
uncompressed for maximum replay speed
or chunked low-level zstd when disk matters
```

Whole-file compression can produce slightly smaller files, but it makes parallel
conversion, partial validation, and corruption isolation worse. The cold format
should preserve enough per-chunk metadata to let workers convert chunks to hot
files independently.
