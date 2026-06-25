# DBN-Style Storage Profile Comparison

Date: 2026-06-25

This comparison is scoped to Aura's DBN-like question: which representation is
best for fixed-width replay, compact storage, and compressed transport. It does
not compare against DBN's public throughput numbers because the machine, data,
record semantics, and book semantics are different.

Artifacts:

- fixtures: `/tmp/aura-dbn-closeout-fixtures/fixtures.json`
- storage matrix: `/tmp/aura-dbn-closeout-storage/sdk_full_matrix_summary.json`

Command:

```bash
target/release/aura_sdk_bench \
  --fixture-dir /tmp/aura-dbn-closeout-fixtures \
  --output-dir /tmp/aura-dbn-closeout-storage \
  --iterations 10 \
  --warmups 2 \
  --batch-size 8192 \
  --datasets sdk-dense,sdk-sparse,sdk-larger \
  --operations sdk-write-aura1,sdk-write-aura0-compact,sdk-write-aura0-hybrid,sdk-read-aura1-batches,sdk-read-aura0-batches,sdk-zstd-aura1-to-aura1,aura1-replay-orderbook-deltas-batch,aura1-replay-orderbook-deltas-apply-batch
```

## Profile Table

| Dataset | Profile / operation | Stored bytes | Raw ratio | Median ms | Records/sec | Recommended use |
|---|---:|---:|---:|---:|---:|---|
| sdk-dense | Aura1 raw write | 82,401 | 1.00x | 54.497 | 75.16K | Fixed-width replay artifact |
| sdk-dense | Aura1 raw read | 82,401 | 1.00x | 0.636 | 6.44M | SDK batch compatibility reads |
| sdk-dense | Aura1.zstd L3 decode | 28,144 | 2.93x | 0.108 | 37.88M | Small fixed-width transport/storage baseline |
| sdk-dense | Aura0 compact write | 15,740 | 5.24x | 55.915 | 73.25K | Cold semantic storage |
| sdk-dense | Aura0 compact read | 15,740 | 5.24x | 0.709 | 5.78M | Cold read when semantic compactness matters |
| sdk-dense | Aura0 hybrid write | 72,039 | 1.14x | 56.745 | 72.18K | Verification/fallback plus byte lane |
| sdk-dense | Aura1 delta extraction | 82,401 | 1.00x | 0.051 | 80.14M | Hot order-book extraction |
| sdk-dense | Aura1 extract+apply | 82,401 | 1.00x | 0.254 | 16.13M | Full replay benchmark path |
| sdk-sparse | Aura1 raw write | 33,236 | 1.00x | 24.332 | 84.17K | Fixed-width replay artifact |
| sdk-sparse | Aura1 raw read | 33,236 | 1.00x | 0.297 | 6.91M | SDK batch compatibility reads |
| sdk-sparse | Aura1.zstd L3 decode | 14,448 | 2.30x | 0.068 | 30.04M | Small fixed-width transport/storage baseline |
| sdk-sparse | Aura0 compact write | 8,110 | 4.10x | 25.080 | 81.66K | Cold semantic storage |
| sdk-sparse | Aura0 compact read | 8,110 | 4.10x | 0.342 | 5.99M | Cold read when semantic compactness matters |
| sdk-sparse | Aura0 hybrid write | 29,634 | 1.12x | 27.536 | 74.38K | Verification/fallback plus byte lane |
| sdk-sparse | Aura1 delta extraction | 33,236 | 1.00x | 0.033 | 62.94M | Hot order-book extraction |
| sdk-sparse | Aura1 extract+apply | 33,236 | 1.00x | 0.128 | 16.01M | Full replay benchmark path |
| sdk-larger | Aura1 raw write | 655,822 | 1.00x | 463.970 | 70.63K | Fixed-width replay artifact |
| sdk-larger | Aura1 raw read | 655,822 | 1.00x | 4.899 | 6.69M | SDK batch compatibility reads |
| sdk-larger | Aura1.zstd L3 decode | 230,805 | 2.84x | 1.093 | 29.98M | Small fixed-width transport/storage baseline |
| sdk-larger | Aura0 compact write | 85,493 | 7.67x | 490.747 | 66.77K | Cold semantic storage |
| sdk-larger | Aura0 compact read | 85,493 | 7.67x | 5.470 | 5.99M | Cold read when semantic compactness matters |
| sdk-larger | Aura0 hybrid write | 553,738 | 1.18x | 506.979 | 64.63K | Verification/fallback plus byte lane |
| sdk-larger | Aura1 delta extraction | 655,822 | 1.00x | 0.253 | 129.76M | Hot order-book extraction |
| sdk-larger | Aura1 extract+apply | 655,822 | 1.00x | 2.418 | 13.55M | Full replay benchmark path |

## Interpretation

- Aura1 raw is the hot replay representation. It is larger than compact Aura0
  but supports predictable fixed-width access and fast order-book extraction.
- Aura1.zstd L3 is the DBN-style compressed fixed-width baseline. It compresses
  raw Aura1 by 2.30x to 2.93x on these fixtures while preserving a simple
  decode-to-fixed-bytes path.
- Aura0 compact is the cold semantic layer. It is smaller than Aura1.zstd on
  these SDK fixtures, especially `sdk-larger`, but replay requires expansion
  back to row/batch semantics.
- Aura0 hybrid is intentionally not the smallest profile. It carries semantic
  streams plus a byte lane so readers can choose speed or verification.
- Aura1.lz4 is not implemented as a standalone `.aura1.lz4` profile. LZ4 exists
  in Aura0 byte-lane profiles; adding Aura1.lz4 should be a separate storage
  profile patch if needed.
- Grimoire huff/nohuff artifacts were not available in this closeout workspace,
  so the matrix is limited to the SDK fixtures.
