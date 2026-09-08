# W03 — transmutation and the size/speed frontier

The tested codec change removes **95.35% of Bitget conversion time** and
**94.29% of OKX conversion time** on the frozen current-generation samples.
It changes no Aura0 archive bytes. Keep compact Aura0 plus its required receipt
as the cold archive default; use cached Zstd-3-compressed Aura1 when faster
repeated replay warrants the additional storage. No new entropy profile is
recommended.

**IMPLEMENTED, VERIFIED, PUSHED:** codec commit
`c8a4dba033f919bd6c0586909e7f83777d1e4f46`; benchmark-reporting commit
`dc6b352feb313830ca322acfe26a4d83feb86d97`. **INTEGRATED:** local combined SDK
`e88f8ea4a596788e4e7921f7a34543c7436adb2f`, whose all-target tests passed in
w01's host validation. **ACTIVE:** no runtime activation or service change by
w03. The entropy variants remain **EXPERIMENTAL**.

## What changed and why

`records::try_compile_explicit_i64_events` previously cloned decoded event
vectors, searched the compact codecs again, encoded a compact body, and then
discarded that body when producing Aura1. Aura1 now retains the already
validated source plan and writes its fixed records and event sidecar.
`generic_planner::decode_generic_i64_events_body` also decoded every stream
twice and copied framed bodies; it now checks frame limits first, decodes once,
and reuses the values for the existing row reconstruction.

The [path audit](path-audit.md) maps every retained validation. The patch keeps
schema/relationship authorization, count/body/value limits, checked arithmetic,
field validation, empty events and event ordering. Hybrid conversion clears
obsolete embedded-lane descriptors. For these two modern samples, preserving
the source plan makes the Aura1 footer respectively **10 and 4 bytes larger**
than the old replanned footer. Fixed-record layout and event facts are unchanged;
this is not a claim of identical whole-file Aura1 bytes across SDK revisions.
The archive-size comparisons below always use the same complete candidate
Aura1 output for Aura0 and Zstd routes.

An isolated diagnostic reproduced **18.28 s** in the discarded generic event
encoder versus **0.92 s** in event decoding. That diagnostic ran under later
compiler load and is supporting profiling evidence, not an additive clean
stage breakdown. The clean before/after measurements establish the gain.

## Controlled current-format results

Ryzen 7 5800X, 128 GB RAM, Rust 1.97.1, release build, one codec thread. Files
were staged on NVMe and preloaded for these memory-to-memory measurements.
The actual host flock was held and derived converters were confirmed stopped;
collectors remained active. The original library was the deployed
`cc04f75c9217e6c6a575145ab0ed98df232f622d`, not the obsolete unrelated local
`main` at `420052c`.

The old binary emitted aggregates only, so three separate processes each ran
one warmup and one measured iteration. The candidate ran one warmup and three
measured iterations. Every value, command, binary hash, CPU/RSS observation,
source/output hash and denominator is retained in [results.json](results.json).

| Conversion workload | Events / level updates | Old median (range), s | Candidate median (range), s | Time reduction |
|---|---:|---:|---:|---:|
| Bitget delta, primary | 105,576 / 738,656 | 18.406 (18.389–18.543) | 0.857 (0.851–0.893) | 95.35% |
| OKX delta, disjoint | 76,108 / 564,384 | 12.667 (12.548–14.342) | 0.723 (0.666–0.745) | 94.29% |

Time reduction is `(old median − new median) / old median`. The first modern
Bitget process crossed the production-resume boundary and was excluded and
replaced. Earlier historical/toy screening is also excluded from accepted
performance claims.

| Current workload | Complete Aura0 bytes | Complete Aura1 bytes | Zstd-3 Aura1 bytes | Aura0→Aura1 ms | Zstd→same Aura1 ms |
|---|---:|---:|---:|---:|---:|
| Bitget snapshot | 4,257 | 84,683 | 6,361 | 0.366 | 0.033 |
| Bitget delta | 3,517,675 | 121,986,684 | 7,608,091 | 856.609 | 48.822 |
| Bybit snapshot | 13,796 | 437,888 | 23,733 | 1.114 | 0.128 |
| Bybit delta | 5,122,129 | 164,173,444 | 12,179,804 | 1,185.150 | 76.220 |
| Binance delta | 1,972,333 | 151,031,839 | 5,004,353 | 981.218 | 238.338 |
| OKX delta | 1,891,043 | 96,587,784 | 4,451,304 | 722.888 | 38.106 |

Aura0 wins file size on all six; Zstd wins byte expansion on all six. Delta
Aura0 files are **53.76–60.59% smaller**, with Zstd bytes as the size denominator.
These two choices form the demonstrated size/expansion frontier. Throughput in
JSON uses both event and level-update counts, plus complete logical output
bytes. No stored-input-byte denominator is substituted for output throughput.

The complete generation totals count the shared restoration receipt once:

| Generation | Aura0 + receipt bytes | Zstd-3 Aura1 + same receipt bytes | Receipt bytes |
|---|---:|---:|---:|
| Bitget | 3,536,184 | 7,628,704 | 14,252 |
| Bybit | 5,150,664 | 12,218,276 | 14,739 |
| Binance | 1,979,269 | 5,011,289 | 6,936 |
| OKX | 1,898,846 | 4,459,107 | 7,803 |

Header/body/footer/trailer accounting is explicit in JSON. The alternate
storage route is research, not an activated replacement for the production
receipt/offload protocol. Original files and receipts remain retained.

## Bounded entropy and candle results

These late probes held both the benchmark lock and all four existing encoder
slots, but an unrelated RustDesk package build ran outside that lock. Their
**sizes and exact decoding checks are valid; their times are contended
observations, not controlled performance gains**.

On Bitget, Zstd-19-compressed Aura1 is **5,613,065 bytes**, still larger than
Aura0. Its one setup compression took 78.7 s in the fair runner; a separate
probe observed 55.3 s. Zstd-3 setup in that probe took 0.134 s. Creation starts
from complete Aura1 for these compression numbers; the cost to create Aura1
must also be charged. SDK Fast creation from decoded Bitget events took 1.77 s
and reproduced the exact original Aura0 hash.

Zstd-19 over Aura0 reduced Bitget from 3,517,675 to **3,496,812 bytes: 0.59%**.
This small residual says nothing about decoder optimality: the unchanged
archive became about twenty times faster to transmute after removing wasted
work.

The complete-container pair kept the same previous-value residual transform
on one existing stream. Selection used entropy headroom, excluded constants,
and tried only packed dictionary and the existing Huffman facility:

| Representation | Complete Aura0 bytes | Change from production |
|---|---:|---:|
| Production | 3,517,675 | — |
| Same structural transform + packed dictionary | 3,635,839 | +3.36% |
| Same structural transform + Huffman dictionary | 3,530,961 | +0.38% |

Huffman saves **2.88% against the packed candidate**, but remains larger than
production. Both variants decoded to identical complete source events. Their
conversion medians were 0.870/0.897 s under contention; no clean speed benefit
was established. Retain the current automatic choice. The change affects the
cost to reach Aura1; it does not optimize the fixed-record parser. The
[structural audit](structural-audit.md) also distinguishes V2 and V3 byte-200
semantics and records why broader relationship searches were not reopened.

Real ES/NQ candlesticks were selected from retained 2010 Parquet, preserving
all ten columns, source order, exact nanoprices/timestamps, integer bounds and
symbol identities. Type/scale/symbol metadata is included below; retained
sourcefacts JSON is verification material, not required archive data.

| Workload | Rows | Aura0 + metadata bytes | Zstd-3 Aura1 + metadata bytes | Zstd-19 Aura1 + metadata bytes |
|---|---:|---:|---:|---:|
| ES primary | 16,384 | 68,364 | 201,431 | 147,387 |
| NQ disjoint | 16,384 | 66,206 | 210,081 | 166,419 |
| ES tiny | 16 | 6,086 | 5,949 | 5,915 |

Aura wins candle archive size on the two larger samples and loses on tiny16.
Contended observations: larger Aura0 creation about 1.0 s, expansion 1.48–1.55
ms versus Zstd-3 0.88–0.92 ms; all-field pipeline 2.58–2.61 ms versus
1.81–1.91 ms. Complete values, three repetitions and ranges are retained.
The expensive Bitget creation/stage/Zstd diagnostic used one measured repetition
and no warmup to bound level-19 work; candle probes used one warmup and three
measured repetitions. Candle preservation is a full-column semantic contract, not restoration of
original Parquet pages/statistics. Original Parquet remains available.

## Correctness, replay and limitations

- 48 focused release regressions passed; the combined SDK all-target suite
  passed separately in w01's integration validation.
- All six current Aura0→Aura1 outputs passed maintained row/schema checks.
  W04 independently compared complete restored rows with retained Parquet and
  checked the actual `grimoire_book::replay::BookState::apply` maps at every
  4,096 events and at completion. All 88 matched cases passed. This independently
  covers the disjoint samples; it is not merely a hash comparison.
- Binance/OKX disjoint generations have no initial snapshot: their replay
  checkpoints are partial-book checks, not proof of a fully initialized book
  or sequence continuity. All retained sequence fields remain present.
- The probe times Aura0→Aura1→full-field consumption and Zstd→Aura1→the same
  consumer, including decoded-object disposal. W04 measures actual book replay
  from the same frozen Aura1 files separately. No summed stage medians are
  presented as a measured cold-to-BookState pipeline.
- Warm-file variants include reads and buffered writes/close, not fsync or a
  cold-disk claim. `wait4` peak RSS includes setup, reference buffers and warmups;
  per-stage probe RSS is before/after state, not allocator instrumentation.
- A separately timed practical canonical-Parquet route was not completed.
  Retained Parquet bytes are context, not a scored alternative. This limits the
  recommendation to the measured Aura0/Aura1 routes.
- Coverage is bounded, not an intraday-stratified or exhaustive compression
  survey. No experimental format, cache, collector or service was activated.

## Reproduce and handoff

The frozen private corpus and raw reports are under
`/home/anton/Downloads/lean-node-20260907/{corpus-w03,results-w03}`. Public Git
contains code, counts, hashes and measurements, not raw data or receipts.

Run the maintained fair benchmark on the frozen current corpus:

```sh
python3 docs/campaigns/lean-node-20260907/w03-transmute/run_fair.py \
  --binary /home/anton/Downloads/lean-node-20260907/bin-candidate/aura-bench \
  --manifest /home/anton/Downloads/lean-node-20260907/corpus-w03/current/manifest.json \
  --output /home/anton/Downloads/lean-node-20260907/results-w03/repeat \
  --commit c8a4dba033f919bd6c0586909e7f83777d1e4f46 --levels 3
```

`run_fair.py` acquires the host flock. [run_remaining.py](run_remaining.py)
reproduces the bounded candle/entropy/creation probes and additionally reserves
all four existing encoder slots. Its exact executed commands are preserved in
JSON. The one-command resume for unresolved timing qualification is:

```sh
python3 docs/campaigns/lean-node-20260907/w03-transmute/run_remaining.py
```

Run that only in a scheduled interval without unrelated heavy builds. The
remaining canonical-reader comparison is an explicitly unmeasured follow-up,
not a prerequisite for the verified codec improvement. No optional jobs remain
running at this handoff.
