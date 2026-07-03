# Byte-Lane Codec Strategy

Status: RESEARCH COMPLETE

## Conclusion

LZ4 is the best compressed speed lane in the current benchmark. Raw is the
absolute fastest if size is ignored. Zstd9 is a useful compact byte-lane option:
it is much smaller than LZ4 and still beats external zstd L3 in the current
fixtures, but encode/write cost has not been measured enough to make it the
default fast profile.

## Current Evidence

Current real-file source:
`/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z`.

| Dataset | Lane | Bytes | Median | Output MiB/s |
| --- | --- | ---: | ---: | ---: |
| huff | fast raw | 36,419,472 | 24.753 ms | 1402.8 |
| huff | fast lz4 | 9,795,104 | 44.373 ms | 782.6 |
| huff | fast zstd9 | 3,277,222 | 55.507 ms | 625.6 |
| nohuff | fast raw | 36,403,778 | 25.271 ms | 1373.8 |
| nohuff | fast lz4 | 9,781,060 | 43.281 ms | 802.1 |
| nohuff | fast zstd9 | 3,264,730 | 58.556 ms | 592.9 |

Compact semantic Aura0 remains much smaller but slower for byte-output
expansion:

- huff: 1,879,040 bytes, 80.608 ms.
- nohuff: 2,246,910 bytes, 104.173 ms.

## Current Implementation Behavior

The real byte lane uses `lz4_flex::compress_prepend_size` and
`decompress_size_prepended`, so it is LZ4 block-style behavior, not an LZ4 frame.
The first serialized implementation uses one whole-file block described by the
`AUBL` footer extension. Future per-block tuning should use the same descriptor
shape for bounded memory, random access, and corruption isolation.

## Recommended Defaults

- Default fast codec: `lz4`.
- Initial block size to benchmark next: `1 MiB`.
- Keep `raw` as a speed-limit/debug profile, not default.
- Keep `zstd9` as a compact byte-lane candidate.
- Hybrid profile should be compact semantic lane plus lz4 byte lane.

## Rejected Ideas

- Raw as default: fastest, but effectively stores full Aura1 bytes.
- Zstd9 as default fast codec: excellent size and adequate speed, but encoding
  cost is unmeasured and lz4 is faster for the current target.
- Whole-body block as the only long-term layout: it prevents useful independent
  block decode and bounded recovery.
- LZ4 frame as the immediate format: AURA already needs footer block
  descriptors, so LZ4 block payloads with AURA-owned metadata are simpler and
  avoid duplicated framing.

## Required Next Benchmarks

After the single-block real file support, run:

- lz4 block sizes: 64 KiB, 256 KiB, 1 MiB, 4 MiB, whole body.
- zstd1/zstd3/zstd9 with the same block sizes.
- raw with the same block sizes.

JSON should include:

- `byte_lane_block_size_requested`
- `byte_lane_block_format`
- `byte_lane_block_count`
- total descriptor/index bytes
- per-block compressed/uncompressed min/p50/p95/max
- checksum/guard mode
- sequential decode throughput
- random single-block decode latency
- encode/compression time
