# Aura benchmarks

The public commands use synthetic inputs. Private retained samples establish
bounded workload results, not universal compression or replay claims. The
[format guide](FORMAT.md) defines V2/V3 and the caller's exactness obligations.
CLI `--help` is the authoritative option reference; historical command matrices
are linked below instead of maintaining a second copy of every flag.

## Public reproduction

```bash
bash scripts/check-onboarding.sh
```

This creates and checks a small SDK fixture, transmutation and real all-field
parsing. Its tiny timings are smoke evidence, not throughput claims. For a
larger generated matrix:

```bash
cargo build --locked --release --lib --bins --examples -j 2
fixture_dir=$(mktemp -d)
target/release/aura-fixture-gen --output-dir "$fixture_dir" --zstd-level 3
target/release/aura-sdk-bench --fixture-dir "$fixture_dir" \
  --output-dir "$fixture_dir/results" --datasets sdk-larger \
  --iterations 3 --warmups 1 --batch-size 8192
```

The fixture inventory is `fixtures.json`. `tiny` is a 16-row OHLCV case and
`sdk-tiny` is a different four-row SDK case. Generated Huffman-heavy data remains
explicitly unsupported: the public writer's speed gate does not select Huffman
for that generated fixture. Never relabel an unsupported row as a pass.

For grouped V3 research, the small diagnostic shares synthetic fixtures with
the writer's independent pre-cleanup SHA regressions:

```bash
cargo run --locked --release --example grouped_search
```

This reports complete candidate costs and three measured iterations after one
warmup. The four tiny cases diagnose search behavior; they do not predict
production throughput. Changing the candidate policy requires the focused
`v3_plan_v2_planned_grouped` tests and `v3_planned_grouped_writer` golden check.

## Measurement boundaries

Use release builds, fixed inputs and executable hashes, matched work, bounded
warmups/repetitions and the same host lock as other heavy jobs. The retained
W03 runner reserves the existing four encoder slots as well; do not wrap a
runner that owns the lock in another lock on the same file. Keep collectors and
transport active and disclose ordinary host load. An unavailable slot is a
measurement limitation, not a speed result.

Report complete header/body/footer/trailer plus required schema, scale, symbol,
occurrence and restoration metadata. Separate archive creation, expansion,
field consumption, adapter conversion and actual book application. Time the
complete pipeline directly; never add independent stage medians to invent it.
Use logical output bytes, rows/events and level updates as denominators.

Memory-resident input, warm file reads, buffered write/close and cold storage
are different conditions. Warm file timings here include read and write/close,
without fsync or OS-cache eviction. Linux `wait4` records complete child-process
peak RSS, including setup/reference buffers; before/after `VmRSS` samples are
not peaks. Exact values, schema, nulls and event boundaries must pass independently
of an encoder/decoder hash agreement.

## Current bounded results (2026-09-08 HOME)

Ryzen 7 5800X, Rust 1.97.1, release, one codec thread. W03 rerun used the host
lock and all four existing codec slots, with ordinary services active and no
unrelated compiler observed. One warmup and three measured iterations. The
initial reservation attempt timed out with **zero measurements**. The successful
retry retained the original ES/NQ identities and all ten columns, exact integer
nanoprices/timestamps, types/scales and symbol dictionary. Values below include
required metadata; sourcefacts/input JSON are independent verification inputs.

| Candle case | Rows | Aura0 + metadata B | Zstd-3 Aura1 + metadata B | Aura0 expand ms | Zstd expand ms | Aura0 expand + consume ms | Zstd expand + consume ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ES | 16,384 | 68,364 | 201,431 | 1.770 | 1.235 | 3.021 | 2.971 |
| NQ | 16,384 | 66,206 | 210,081 | 1.916 | 0.895 | 3.292 | 2.196 |
| ES tiny | 16 | 6,086 | 5,949 | 0.0252 | 0.0061 | 0.0360 | 0.0259 |

Aura0 wins size on these two larger samples and loses on tiny16. Zstd-3 wins
byte expansion. NQ and tiny16 favor Zstd in the full all-field pipeline; ES
medians are close and its sample variation does not establish a useful full
pipeline advantage. This does not generalize a small candle corpus to all data.
Decoded-facts-to-archive creation is directly measured: Aura0 versus
Aura1-and-Zstd3 is 1,275/1,148 ms for ES, 1,251/1,128 ms for NQ and
1.492/1.480 ms for tiny16. Direct creation and transmutation produced the same
complete Aura1 bytes in all three cases. Original Parquet extraction is outside
these creation timers.

The unchanged archived Zstd-19 size results are 147,387 B (ES), 166,419 B (NQ)
and 5,915 B (tiny16), including metadata. Expensive book level-19 compression
was not repeated: Bitget Aura1 was 5,613,065 B versus compact Aura0 3,517,675 B;
Zstd-19 over Aura0 saved only 0.59%.

| Bitget delta representation | Complete Aura0 B | Aura0→Aura1 ms | Expand + all-field consume ms |
| --- | ---: | ---: | ---: |
| Production structural/dual-domain choice | 3,517,675 | 1,134.9 | 1,401.3 |
| Same transform + packed dictionary | 3,635,839 | 974.0 | 1,252.8 |
| Same transform + Huffman dictionary | 3,530,961 | 988.9 | 1,288.7 |

These are one ordered bounded comparison, not an order-balanced speed claim.
Both forced candidates still lose complete-container size; retain the current
selection. Their emitted Aura1 headers and fixed record bodies are byte-identical
to production; only retained footer-plan bytes differ. They do not introduce a
faster book engine. All event values and boundaries passed the existing exact
comparison. Byte 200 alone is not a universal codec: parent permissions,
transforms, nulls and field types remain authoritative.

The existing 14,252-byte restoration receipt payload and unchanged 4,257-byte
snapshot must also be retained for the complete Bitget generation: production,
packed and Huffman totals are 3,536,184 / 3,654,348 / 3,549,470 B. Experimental
bindings would need resealing by Grimoire before publication; these are local
experiments, not production archives. Forced candidate construction costs and
complete process cost are recorded separately from source-to-archive creation.
The direct event-creation follow-up and OKX recheck each hit the bounded host-lock
wait. No timing was invented: the historical OKX regression remains in force.
The corrected creation probe uses Full search for Aura1 because Fast is supported
only for Aura0. [Exact samples, rates, memory and identities](benchmarks/readiness-20260908.json)
include these explicit limits.

Compact Aura0 fits cold archives where size matters and restoration is less
frequent. A cached complete Aura1 or its Zstd-3 representation can trade storage
for repeated replay. No cold-disk latency, production capacity, or universal
Zstd superiority is established by these memory/warm-file measurements.

## Private reproduction commands

Resolve retained paths locally; originals are not distributed. `corpus_root`
is the existing W03 corpus, `result_dir` a **new directory outside source**,
and `sdk_revision` the exact built revision. The existing runner owns its locks:

```bash
python3 docs/campaigns/lean-node-20260907/w03-transmute/run_remaining.py \
  --corpus-root "$corpus_root" --bin-dir "$PWD/target/release" \
  --output "$result_dir" --commit "$sdk_revision" --question candles
```

Use the same command with `--question entropy` for the one-stream structural,
packed and Huffman comparison. Use `--question creation --iterations 1 --warmups 0`
for the bounded Bitget source-facts creation question. Level-19 size evidence is
already retained; do not repeat its expensive compression merely to rediscover it.

Actual order-book replay belongs to the maintained Grimoire runner:

```bash
python3 "$grimoire/experiments/aura1_replay_campaign.py" \
  --manifest "$replay_manifest" --raw-baseline "$raw_baseline" \
  --raw-candidate "$raw_candidate" --book-baseline "$book_baseline" \
  --book-candidate "$book_candidate" --output "$result_dir"
```

It uses the real `BookState::apply`, with separate decode/restoration, adapter
and application timers, complete pipeline timing, exact retained-source values
and checkpoint maps. Coordinate slot reservation with the Grimoire owner. The
88-case W04 correctness evidence remains valid for unchanged replay code.
Its reported OKX warm-file 7.1% regression is not erased by faster decoding.

## Historical benchmark evidence

The [frozen prior benchmark guide](https://github.com/curvefit/aura/blob/dfd4c0484d7c81e39a2dabd5feffff9c6212d5a4/docs/BENCHMARKING.md)
retains CLI matrices, shared-field analysis, materialized fallback, byte-lane,
SDK matrix and selected/all-field results. Those measurements are scoped to
their original revisions and inputs. Current focused sources:

- [W03](campaigns/lean-node-20260907/w03-transmute/report.md): complete Aura0/Zstd size frontier and archived Zstd-19 results.
- [W04](campaigns/lean-node-20260907/w04-replay/report.md): actual consumer, 88 exact cases and explicit OKX regression.
- [W05](campaigns/lean-node-20260907/w05-aura-cleanup/report.md): prior onboarding and dependency cleanup.

Older flat/fused replay intentionally omits some full-book facts and is not an
equivalent consumer for W04 retained generations. Historical source identities
remain useful; deployment and old next-action prose can be superseded by merges.
