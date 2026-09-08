# Aura1 event decoding and actual book replay

The reusable-row decoder reduces median buffer-resident full replay time by
5.1–11.1% on the four frozen generation workloads. This is a measured tradeoff:
OKX raw decoding is 7.3% slower and its warm-file full replay is 7.1% slower.
The other three warm-file replay medians improve by 1.2–11.0%. All 88 cases pass
exact value, event-boundary and applicable source/book-state checks.

## Changes and maintained boundaries

- Aura `cad9e07`: `decode_i64_events_file` decodes Aura1 directly into the existing
  fully materialized `I64Event` result with one reusable row buffer. It avoids the
  complete flattened `Vec<Vec<i64>>` intermediate. The SDK reader's mandatory
  explicit-event validation also benefits. No new unsafe code or stored layout.
- Grimoire `53857f2`: additive `full_book_aura::decode_kind_replay` restores full
  kind-separated Aura1 into `BookContainerRow`, using the existing metadata,
  schema, scale, null-mask and source-ordinal validation. Aura0 `verify_kind`
  retains its independent verification and profile restriction.
- The maintained consumer benchmark converts exact scaled integers to
  `ParsedMessage` and calls the actual `replay::BookState::apply`. It preserves
  all three nullable quantity lanes, order counts and explicit deletion flags.
  There is no alternate book mutation algorithm in the benchmark.

The already-maintained flat/fused Aura path was not reimplemented. It deliberately
omits metadata fields and explicit event boundaries, so it is not an equivalent
consumer for these full retained book generations. No format change or external
record-format rewrite was necessary. Parquet is an independent correctness
reference, not a timed compression competitor.

## Corpus and method

`corpus.json` records exact source/output identities, counts, schema and received
UTC ranges. Bitget and Bybit are the primary retained two-file generations with
snapshots and deltas. Binance and OKX are disjoint retained one-file delta
windows. These provide different observed message densities (approximately
862/3,724 and 1,111/4,758 events/s respectively), not archive-wide quiet/busy
quantiles. Tiny snapshot files and realistic 93–157 MiB Aura1 delta outputs are
separate cases. Original Parquet and Aura0 files remain retained. W03's flat
synthetic candlestick workload is separate and unaffected by this event decoder;
no candle throughput is attributed to this patch.

Baseline decoder: `cc04f75c9217e6c6a575145ab0ed98df232f622d`, verified remote main
and deployed SDK at discovery. The original clean checkout at `420052c` was
obsolete divergent history and was preserved. Aura1 references were produced by
w03 `c8a4dba` and both decoder variants consumed the **same frozen bytes**.
Their complete events, schema and header facts match independently decoded Aura0.

Host: Ryzen 7 5800X, 128 GiB, Linux, Rust 1.97.1 / LLVM 22.1.8, Cargo 1.97.1.
Measured replay is single-threaded; builds use two Cargo jobs. All heavy w04
jobs use the canonical campaign lock. W01 stopped derived workers at 01:10 UTC;
read-only checks confirmed both derived worker services inactive with MainPID 0
at 01:11 and during the sweep at 01:28. The user-service journal records their
next start at **01:31:20 UTC**, after the final measured case ended at 01:30:54;
the earlier approximately-01:28 resume notice was not the actual service start.
Collectors, receive services, API and
other ordinary desktop/production load remained active. The matched cases ran
2026-09-08 01:24:58–01:30:54 UTC after waiting for the integration build.

Each case has one warmup and three measured repetitions; baseline/candidate
order alternates across cases. `current-summary.json` retains every individual
wall/CPU value, median, minimum, maximum, denominators, stage times and RSS scope.
The file mode rereads each input and is explicitly **warm OS-cache**; no cold-disk
claim. The explicit-event APIs still load/materialize the complete input/result.
Open timing charges the actual SDK ownership and required validation work.
First-event latency is eager full materialization, separately recorded before
minimal consumption. CPU uses Linux schedstat; zero readings for sub-millisecond
snapshot cases are below its observed update resolution, not zero CPU work.

## Complete usable-record decoding, memory

| Sample | Events | Level updates | Baseline ms | Candidate ms | Time reduction |
| --- | ---: | ---: | ---: | ---: | ---: |
| bitget_snapshot | 3 | 482 | 0.140 | 0.115 | +18.23% |
| bitget_delta | 105,576 | 738,656 | 286.002 | 247.419 | +13.49% |
| bybit_snapshot | 30 | 2,692 | 0.568 | 0.418 | +26.55% |
| bybit_delta | 187,792 | 1,064,800 | 454.884 | 400.028 | +12.06% |
| binance_delta | 37,817 | 843,525 | 259.310 | 243.085 | +6.26% |
| okx_delta | 76,108 | 564,384 | 153.905 | 165.165 | -7.32% |

Positive percentages are `(baseline median − candidate median) / baseline median`.
Negative entries are regressions. Minimal consumption also decodes every event and
black-box consumes every required field; its independent results are in the JSON.
The full production schema gains are smaller than the historical screening
projections in `baseline.json` / `screen.json`; the screening numbers are not
substituted for the current corpus.

## Actual full replay

| Generation | Memory baseline → candidate ms | Memory time reduction | Warm-file baseline → candidate ms | Warm-file time reduction | Candidate memory events/s | Candidate memory levels/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| bitget | 949.72 → 865.88 | +8.83% | 992.99 → 981.29 | +1.18% | 121,932 | 853,623 |
| bybit | 1506.49 → 1372.44 | +8.90% | 1556.38 → 1462.39 | +6.04% | 136,853 | 777,807 |
| binance | 1347.58 → 1197.72 | +11.12% | 1441.94 → 1282.62 | +11.05% | 31,574 | 704,273 |
| okx | 754.02 → 715.59 | +5.10% | 734.06 → 785.86 | -7.06% | 106,357 | 788,695 |

The decoder/restoration stage improves in all eight book cases. For example,
Bybit memory decoding/restoration falls from 581.6 to 404.9 ms, while its unchanged
adapter and application stages remain around 0.8 s combined. For OKX warm-file,
decoding/restoration falls from 276.9 to 223.2 ms, but adapter time increases from
237.5 to 326.3 ms, producing the overall regression. Allocation/cache effects in
the unchanged downstream code were not separately isolated. This is a decoding
and allocation improvement, not a new book-application algorithm or a universal
speedup. Ranges overlap for some smaller gains; these are three-repetition sample
medians, not a statistically established production-wide forecast.

RSS in the main JSON is sampled after each timed iteration and includes the
retained independent reference and useful output. It is not decoder-only peak
RSS: primary full-replay RSS is approximately unchanged (Bitget 777 MiB, Bybit
1,224 MiB). Source-level accounting shows the flattened child-row allocation and
outer row vector replaced by one scratch row; final event/child output allocations
remain charged. Exact global allocator call/byte counts were unavailable.

A separate locked Linux `wait4` diagnostic measures true peak RSS for one fresh
process per variant on a primary and disjoint workload, including setup,
independent reference decoding, replay and exact-state verification. Bybit is
1,330,704 → 1,330,488 KiB; OKX 705,980 → 706,140 KiB—effectively unchanged.
`peak-rss.json` retains these observations and complete process CPU/wall cost.
This metric does not isolate decoder-only memory; no whole-process peak-memory
reduction is claimed. The maintained runner now captures the same per-child
resource usage for future runs, smoke-checked against the tiny snapshot.

## Correctness and compatibility

All six raw files pass exact Aura0/Aura1 event values, children, boundaries,
schema and header comparisons. Every full-generation consumer run restores rows
exactly equal to its retained source Parquet, restores source ordinal order and
checks contiguity. Replay compares exact message values and exact full book maps
in lockstep every 4,096 events and at the final event, as well as the timed final
maps. Hashes only bind immutable artifacts; they are not the semantic proof.
The runner rehashes inputs after all 88 cases and rejects changed inputs.

The logical replay key is venue, market, symbol, source family and segment;
physical feed remains in each message, allowing REST/bootstrap snapshots and
websocket deltas to share their intended book. Every retained sequence field is
preserved. `BookState::apply` does not enforce continuity, and this benchmark does
not infer it from sequence minima/maxima. Initial state is empty per logical
stream/segment; snapshots reset it. Delta-only or unanchored streams have partial
initial-state coverage, so the results prove equivalent replay from that stated
initial state, not complete historical market books.

Existing fixed-body dimensions, field coverage, empty-event headers, child
counts, repeated event-header equality, truncation and unsupported-version
rejections remain. Multiple independent corruptions can be detected in a
different order. Release checks: 37 Aura tests in `aura1_event_decode`,
`explicit_events`, `decode_bounds` and `repeated_parent_edge_cases`; all four
Grimoire contract/example tests passed with both SDK variants. Cargo's temporary
SDK override was removed and the original lockfile hash restored exactly.

A discovery child ran four receipt verifications without the lock around
00:20:48–00:21:31 UTC despite its assignment. This deviation was disclosed; no
w04 timing above overlaps it. W05's earlier uncoordinated build affected an
unrelated provisional w03 screen, not this final locked sweep.

## Reproduction and handoff

The single maintained command is Grimoire's
`experiments/aura1_replay_campaign.py`; it drives the raw SDK and actual book
consumer with identical binary/data contracts and acquires the lock itself.
Do not wrap it in a second exclusive lock on the same file. Build instructions
are in Grimoire `docs/campaigns/lean-node-20260907/w04-replay/README.md`.
The private immutable corpus manifest and complete per-case reports are retained
in the workspace; public artifacts contain hashes and metrics, not source data.

IMPLEMENTED / VERIFIED: Aura decoder and Grimoire reader bridge, maintained
benchmark and exact-source replay comparisons. Grimoire code is pushed to its
private campaign branch. Aura code/report remains local because automatic
approval review rejected the public report push; no indirect publication was
attempted. Integration owner received the exact commits and binaries for local
integration. W04 activated no services and changed no runtime SDK pin, format,
retention or original data.
