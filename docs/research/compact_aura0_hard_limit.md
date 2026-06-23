# Compact Aura0 Hard-Limit Evidence

Status: COMPACT TARGET STILL FAILS AFTER SEVEN EXPERIMENTS

## Target

The product comparison is:

- compact semantic `.aura0 -> .aura1` bytes
- external/fair `.aura1.zst -> .aura1` bytes

Both production paths are measured with no canonical hash, no strict guard, no
row materialization, and the same in-memory output sink. Aura1 byte lanes and
LZ4 controls are excluded from this pass condition.

## Best Measured Compact Results

| Dataset | Path | Median ms | p95 ms | Output MB/s | JSON |
| --- | ---: | ---: | ---: | ---: | --- |
| grimoire-50mb-huff | original compact semantic | 81.811 | 83.222 | 424.4 | `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_materialized.json` |
| grimoire-50mb-huff | best optimized compact sample | 75.251 | 76.064 | 461.4 | `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_materialized_nozerofill_rawappend.json` |
| grimoire-50mb-huff | final sequential optimized compact | 79.703 | 84.847 | 435.7 | `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/final_huff_compact_optimized_seq.json` |
| grimoire-50mb-huff | zstd L3 Aura1 bytes | 67.411 | 71.438 | 515.1 | `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/final_huff_zstd_l3.json` |
| grimoire-50mb-nohuff | original compact semantic | 110.089 | 117.403 | 315.4 | `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_materialized.json` |
| grimoire-50mb-nohuff | best optimized compact sample | 79.386 | 86.210 | 437.3 | `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_materialized_nozerofill_rawappend.json` |
| grimoire-50mb-nohuff | final sequential optimized compact | 88.091 | 94.777 | 394.1 | `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/final_nohuff_compact_optimized_seq.json` |
| grimoire-50mb-nohuff | zstd L3 Aura1 bytes | 66.398 | 68.036 | 522.9 | `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/final_nohuff_zstd_l3.json` |

Even using the best observed optimized compact samples, compact Aura0 still
loses by 7.840 ms on huff and 12.988 ms on no-Huffman against the fresh zstd L3
results. The final sequential samples lose by wider margins.

## What Was Eliminated

The sprint removed or measured these sources of avoidable compact decode work:

1. fair benchmark path plumbing: `--decode-path cursor` now reaches fair byte
   benchmarks;
2. cursor temporary Aura1 body allocation and 36.4 MB copy;
3. cursor generic field push loop for the fixed 46-byte Aura1 row layout;
4. no-Huffman generic `DirectAura1SlotSource` writer dispatch;
5. production zero-fill of the 36.4 MB Aura1 body before overwrite;
6. exact-slot writer specialization, tested and rejected;
7. no-guard offset/slice precomputation rewrite, tested and rejected.

## Remaining Time

After the kept optimizations, compact Aura0 still does work zstd does not do:

- decode 12 semantic streams into 2,754,892 values;
- reconstruct dictionary/group/partition state;
- reconstruct segmented deltas;
- read presence masks and sparse streams;
- pack eight logical fields into each fixed Aura1 row;
- write every Aura1 output byte.

Zstd inflates already-formed Aura1 bytes. It avoids semantic joins, sparse
field reconstruction, stream-to-field mapping, and integer-to-row packing.

## Correctness Evidence

Verify-mode compact fair bytes output matched the reference Aura1 bytes:

- huff output hash: `12194870092346231300`
- no-Huffman output hash: `10372430540135078667`
- JSON:
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/final_huff_compact_optimized_verify.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/final_nohuff_compact_optimized_verify.json`

The targeted row equality and strict guard tests passed after the raw append
writer change.

## Conclusion

This is hard evidence for the current implementation, not a proof that no
future compact format could ever win. The current compact semantic layout still
needs a larger change to beat zstd byte expansion:

- either a faster semantic integer codec with vector-friendly control/data
  separation;
- or direct semantic decode into row stores without materialized stream vectors;
- or footer metadata that permits block-level row-store recipes and stream
  cursor specialization beyond the current generic plan.

The smallest next compact-only patch with plausible payoff is an extracted
stream microbenchmark and replacement prototype for the integer stream codecs
(`BaseBitpack`, dictionary, delta) because writer-only work has been reduced
and measured but does not close the gap.
