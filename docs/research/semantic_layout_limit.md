# Compact Semantic Aura0 Decode Limit

Status: LOSS WITH EVIDENCE

## Product Result

The compact semantic Aura0 profile remains smaller than external Aura1.zst, but
it does not beat the product decode target on the grimoire huff/nohuff fixtures.

Fresh artifacts:

```text
/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/huff_compact_semantic.json
/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/nohuff_compact_semantic.json
/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/huff_external_zstd_l3.json
/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/nohuff_external_zstd_l3.json
```

| Dataset | Compact Aura0 bytes | Aura1.zst L3 bytes | Aura1 bytes | Compact ms | zstd L3 ms | Gap |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| grimoire-50mb-huff | 1,879,040 | 5,542,945 | 36,410,980 | 80.608 | 61.386 | +19.222 ms |
| grimoire-50mb-nohuff | 2,246,910 | 5,538,275 | 36,403,133 | 104.173 | 62.798 | +41.375 ms |

## Why Compact Loses

Compact Aura0 stores semantic streams. Expanding it to Aura1 bytes requires:

- parsing the compiled footer/program;
- decoding multiple semantic streams;
- reconstructing dictionary and delta state;
- reconstructing fields into fixed-width rows;
- writing the Aura1 header/body/footer.

External zstd inflates already-formed Aura1 bytes. It does not perform field
reconstruction, stream joins, dictionary lookup, or per-field Aura1 packing.

## Decision

Compact semantic Aura0 should remain the compact archival/interchange profile.
It should not be advertised as the faster-than-zstd cold byte-expansion profile
for these fixtures.

The speed profile is the real Aura0 byte lane:

| Dataset | Profile | Codec | Bytes | Median ms | zstd L3 ms | Decision |
| --- | --- | --- | ---: | ---: | ---: | --- |
| huff | fast | lz4 | 9,795,104 | 44.373 | 61.386 | wins |
| huff | hybrid | lz4 | 11,665,724 | 43.761 | 61.386 | wins |
| nohuff | fast | lz4 | 9,781,060 | 43.281 | 62.798 | wins |
| nohuff | hybrid | lz4 | 12,027,397 | 44.058 | 62.798 | wins |
