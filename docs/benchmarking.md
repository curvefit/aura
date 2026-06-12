# Benchmarking plan

Aura profiles should be compared with synthetic and private real-world inputs,
but public benchmarks should remain source-neutral.

Measure at least:

```text
encoded bytes per event
encoded bytes per changed level
decode events/sec
decode levels/sec
ingest -> compiled conversion throughput
padding overhead by block size
compression ratio by chunk size and zstd level
```

Benchmark matrix:

```text
level: .aura, .aura0, .aura1
Aura1 block capacity: 1, 2, 4, 8, 16, 32
chunk target: 16 MiB, 32 MiB, 64 MiB
compression: none, zstd low, zstd high
```

Important caveats:

- repeated timestamps can make Aura1 block packing smaller,
- one event per header is the simplest max-speed baseline,
- larger block sizes can parse faster but increase padding,
- outlier events should only pay for their own extra blocks,
- whole-file compression is simpler but blocks parallel conversion.
