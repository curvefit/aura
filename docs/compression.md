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

## OpenZL probe

A one-day BTCUSDT 1m sample was materialized from the current integer-stamp
harness and compressed with the external OpenZL `zli` CLI as a whole-file
baseline. Every decompressed output matched the original bytes.

```text
file    original   openzl serial   openzl train-inline
.aura   128494     68329           62769
.aura0   33723     32115           30480
.aura1   83945     68784           59091
```

The result is directionally useful but not a replacement for chunked Aura
compression. OpenZL helps the raw ingest and hot replay files more than `.aura0`;
after Aura0 has applied candle/residual transforms and bitpacking, whole-file
OpenZL only trims a small amount. Compressed `.aura1` can materialize hot bytes
faster than the current generic `.aura0 -> .aura1` path, but it stores much more
data than `.aura0`. Re-run this comparison after a fused Aura0-to-Aura1
transcoder exists.
