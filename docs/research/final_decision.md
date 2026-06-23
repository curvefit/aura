# Aura0 Speed Target Final Decision

Status: AURA FINAL SPEED TARGET: PASSED_WITH_HYBRID_PROFILE

## Decision

Compact semantic Aura0 remains the smallest archival/interchange profile. It is
not the faster-than-zstd byte-expansion profile on the current grimoire huff and
nohuff fixtures.

The final speed target is met by real Aura0 fast/hybrid byte-lane files, not by
the old benchmark-only prototype. The byte lane is serialized in the compiled
footer with the `AUBL` descriptor table and expanded by the production Aura0
reader path.

## Default Recommendation

- Default Aura0 profile for speed plus semantic fallback: `hybrid`.
- Default byte-lane codec: `lz4`.
- Default byte-lane block shape: current single whole-file Aura1 byte block.
- Default lane selection for Aura0 -> Aura1 bytes: `--use-byte-lane auto`.
- Default guard mode for production timing: `no_guard`.
- Default canonical hash mode for production timing: `none`.
- Strict/verify mode: validate output equality and output-byte guard; use
  `--use-byte-lane always` when proving the byte lane specifically.

Use `fast + lz4` when the semantic lane is not needed. Use `compact` when
minimum file size is more important than faster-than-zstd byte expansion.

## Production Scoreboard

Artifacts:

```text
/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/*.json
```

| Dataset | Path | Bytes | Median ms | p95 ms | Records/s | Output MB/s | zstd L3 ms | Decision |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| huff | compact semantic | 1,879,040 | 80.608 | 82.328 | 9.817M | 430.8 | 61.386 | loses |
| huff | external zstd L3 | 5,542,945 | 61.386 | 63.310 | 12.891M | 565.7 | 61.386 | baseline |
| huff | fast raw byte lane | 36,419,472 | 24.753 | 25.739 | 31.970M | 1402.8 | 61.386 | wins |
| huff | fast lz4 byte lane | 9,795,104 | 44.373 | 45.144 | 17.834M | 782.6 | 61.386 | wins |
| huff | hybrid lz4 byte lane | 11,665,724 | 43.761 | 56.439 | 18.084M | 793.5 | 61.386 | wins |
| huff | fast zstd9 byte lane | 3,277,222 | 55.507 | 56.997 | 14.257M | 625.6 | 61.386 | wins |
| nohuff | compact semantic | 2,246,910 | 104.173 | 118.529 | 7.597M | 333.3 | 62.798 | loses |
| nohuff | external zstd L3 | 5,538,275 | 62.798 | 83.526 | 12.602M | 552.8 | 62.798 | baseline |
| nohuff | fast raw byte lane | 36,403,778 | 25.271 | 25.885 | 31.314M | 1373.8 | 62.798 | wins |
| nohuff | fast lz4 byte lane | 9,781,060 | 43.281 | 45.772 | 18.284M | 802.1 | 62.798 | wins |
| nohuff | hybrid lz4 byte lane | 12,027,397 | 44.058 | 46.933 | 17.962M | 788.0 | 62.798 | wins |
| nohuff | fast zstd9 byte lane | 3,264,730 | 58.556 | 64.099 | 13.513M | 592.9 | 62.798 | wins median |

## Verification Evidence

Verify-mode real fast lz4 runs reported exact output equality:

| Dataset | JSON | output_bytes_equal | output_byte_hash |
| --- | --- | --- | ---: |
| huff | `/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/huff_fast_lz4_verify.json` | true | 12194870092346231300 |
| nohuff | `/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/nohuff_fast_lz4_verify.json` | true | 10372430540135078667 |

## Remaining Risks

- The first real byte lane is a single whole-file block. Per-block descriptors
  are defined but not yet tuned.
- Old binaries that strictly decode `AURP` footers will reject new files with
  the `AUBL` extension; new binaries still read old compact Aura0 files.
- The fastest tail parser intentionally avoids full footer-plan construction in
  production byte-lane expansion. Use strict/verify runs when proving semantic
  equality.
- Allocation counters are still coarse.
