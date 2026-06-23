# AURA0 Speed Target Sprint Log

Current target: make `.aura0 -> .aura1` byte expansion beat `.aura1.zst -> .aura1`
byte expansion for both `grimoire-50mb-huff` and `grimoire-50mb-nohuff`, or
prove the semantic layout needs a format-level speed lane and implement the
smallest working prototype that wins.

Current correction: the byte-lane/LZ4 work is not an acceptable answer for the
compact semantic target. The active target is compact semantic `.aura0 -> .aura1`
bytes only. Byte lanes remain controls.

Each experiment must record:

- hypothesis
- file/function targeted
- expected speedup
- patch summary
- commands run
- benchmark JSON paths
- result
- keep/reject decision
- next implication

## Experiment 0: Preflight

- hypothesis: The branch starts from a clean, passing baseline.
- file/function targeted: repository state only.
- expected speedup: none.
- patch summary: none.
- commands run:
  - `git status --short --branch`
  - `git worktree list`
  - `git rev-parse HEAD`
  - `git diff --stat`
  - `git diff --check`
  - `cargo test`
  - `cargo build --release --bin aura-bench`
- benchmark JSON paths: none.
- result: clean branch `final-format-resolution` at `4dafd917411fbdd6885f2b6756f46ba434f15d70`; tests and release build passed.
- keep/reject decision: keep baseline.
- next implication: proceed with benchmark truth, speed-limit probes, and speed-lane prototype work.

## Compact Experiment C0: Research and Experiment Ranking

- hypothesis: Compact Aura0 loses because it reconstructs semantic fields while
  zstd inflates already-formed Aura1 bytes; the next patches must remove
  interpretation/materialization from the compact path.
- prior-art basis: DBN fixed-width/zero-copy design; zstd frame/block byte
  inflation; Stream VByte and BP128-style integer decode research.
- file/function targeted: research only; no production code.
- expected speedup: none directly; constrains experiments.
- patch summary: added `docs/research/compact_aura0_decode_research.md`.
- commands run:
  - `git status --short --branch`
  - `git rev-parse HEAD`
  - `git diff --stat`
  - `git diff --check`
  - `cargo test`
  - `cargo build --release --bin aura-bench`
- benchmark JSON paths: none.
- result: ranked compact-only experiments created.
- keep/reject decision: keep.
- next implication: run fresh compact materialized/cursor/zstd baseline, then
  patch the largest measured compact bottleneck.

## Compact Experiment C1: Fresh compact baseline and cursor wiring

- hypothesis: The existing `--decode-path cursor` flag was not measuring the
  cursor path for fair compact byte-output benchmarks.
- prior-art basis: the dataflow archaeology found the fair operation hard-coded
  `Aura0DecodePath::Materialized`.
- file/function targeted:
  - `src/bin/aura_bench.rs::fair_aura1_bytes_operation`
  - `tests/aura_bench_cli.rs::aura_bench_applies_decode_path_to_fair_aura0_bytes`
- expected speedup: none by itself; makes cursor experiments measurable.
- patch summary: pass `decode_path` from `run_operation` into the fair byte
  operation and add a CLI regression test for path plumbing.
- commands run:
  - `cargo test --test aura_bench_cli aura_bench_applies_decode_path_to_fair_aura0_bytes -- --nocapture`
  - `cargo build --release --bin aura-bench`
  - fresh huff/nohuff materialized/cursor/zstd benchmark commands under the JSON paths below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_materialized.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_cursor_after_wiring_probe.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_zstd_l3.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_materialized.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_cursor.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_zstd_l3.json`
- result: cursor now activates on huff (`direct_cursor_stream_count=12`) but is
  much slower before writer specialization: huff cursor 176.782 ms vs
  materialized 81.811 ms and zstd L3 62.313 ms.
- keep/reject decision: keep harness fix; reject current cursor as default.
- next implication: optimize cursor writer internals, not benchmark plumbing.

## Compact Experiment C2: Cursor fixed-row writer and direct final output

- hypothesis: Existing cursor mode is slow because it pushes fields generically
  into a temporary body and copies that body into the final Aura1 file.
- prior-art basis: DBN/fixed-width lesson: common replay rows should use
  predictable fixed stores.
- file/function targeted:
  - `src/generic_planner.rs::try_encode_generic_i64_aura1_body_streaming`
  - `src/records.rs::try_compile_aura0_to_aura1_fast_profiled`
- expected speedup: remove most of the cursor writer gap and eliminate 36.4 MB
  temp/copy from cursor mode.
- patch summary: add a cursor writer variant that appends to caller-owned output
  and use the existing fixed 46-byte row store when the plan matches the
  partitioned sparse Aura1 layout.
- commands run:
  - `cargo test --test aura_bench_cli aura_bench_applies_decode_path_to_fair_aura0_bytes -- --nocapture`
  - `cargo build --release --bin aura-bench`
  - huff/nohuff cursor benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_cursor_fixedrow.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_cursor_directout.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_cursor_fixedrow.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_cursor_directout.json`
- result: cursor improved but still loses. Huff cursor: 176.782 -> 132.718
  fixed-row -> 128.901 direct-output, still slower than 81.811 materialized.
  Nohuff cursor direct-output: 115.964 ms, still slower than 110.089
  materialized and far slower than 61.636 zstd L3. Direct-output counters show
  `temporary_buffer_bytes=0` and `copied_bytes=0`.
- keep/reject decision: keep behind `--decode-path cursor` only; reject as
  default compact path.
- next implication: materialized writer/source path is still the best default.

## Compact Experiment C3: Zero-fill elimination in materialized writers

- hypothesis: materialized writer spends about 20 ms zero-filling the Aura1 body
  before every byte is overwritten.
- prior-art basis: fixed-width replay target should avoid redundant passes over
  output bytes.
- file/function targeted:
  - `src/generic_planner.rs::try_write_partitioned_sparse_i64_aura1_body_inner`
  - `src/generic_planner.rs::try_write_generic_i64_aura1_body_from_streams_inner`
- expected speedup: 10-20 ms if zero-fill is a real separate pass.
- patch summary: temporarily replaced fixed-row `resize(..., 0)` with
  `reserve` + `set_len` after proving each fixed row is overwritten.
- commands run:
  - `cargo test --test writer_reader_api aura0_to_aura1_default_fast_path_matches_column_fallback -- --nocapture`
  - `cargo test --test writer_reader_api aura0_to_aura1_fused_output_guard_matches_full_scan -- --nocapture`
  - huff/nohuff materialized benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_materialized_nozerofill_both.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_materialized_nozerofill_both.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_materialized_nozerofill_verify.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_materialized_nozerofill_verify.json`
- result: correctness passed and verify output equality was true, but performance
  was mixed. Huff 81.811 -> 80.082 ms with p95 83.222 -> 95.169 ms. Nohuff
  110.089 -> 104.263 ms with p95 117.403 -> 121.806 ms.
- keep/reject decision: rejected and reverted for default path because p95
  regressed and median did not come close to zstd.
- next implication: remove interpretation/source dispatch rather than only
  output initialization.

## Compact Experiment C4: Materialized streaming-config writer

- hypothesis: the no-Huffman fixture loses extra time in
  `DirectAura1SlotSource::value_at` dispatch and source construction after
  streams are already materialized.
- prior-art basis: DBN-style replay keeps row packing table-driven; fixed
  layout rows should not call a generic source state machine for every field.
- file/function targeted:
  - `src/generic_planner.rs::try_write_generic_i64_aura1_body_from_streams_inner`
  - `src/generic_planner.rs::try_write_streaming_config_i64_aura1_body_from_streams`
- expected speedup: 15-25 ms on no-Huffman by avoiding generic per-row source
  dispatch and source vector setup.
- patch summary: added a conservative materialized-stream writer using
  `StreamingAura1Config`; it fetches stream slices once, walks partition/event
  runs directly, and falls back for unsupported footer plans.
- commands run:
  - `cargo check`
  - `cargo build --release --bin aura-bench`
  - `target/release/aura-bench --operation aura0-to-aura1-bytes ... --decode-path materialized`
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_materialized_streaming_fastwriter_repeat.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_materialized_streaming_fastwriter_repeat.json`
- result: no-Huffman improved from 110.089 ms to 80.625 ms in the best repeat;
  writer stage dropped from 72.452 ms to 49.425 ms. Huff did not use this
  path and remained in the same range.
- keep/reject decision: keep as a compact semantic optimization.
- next implication: output zero-fill remains visible inside writer timing.

## Compact Experiment C5: Exact-slot writer specialization

- hypothesis: specializing the C4 writer to the exact 8-slot grimoire layout
  would beat the generic `StreamingAura1Config` slot loops.
- prior-art basis: fixed-width replay benefits from hardcoded row-store recipes
  once schema compatibility is proven.
- file/function targeted:
  - `src/generic_planner.rs::try_write_exact_streaming_partitioned_sparse_i64_aura1_body_from_streams`
- expected speedup: 3-8 ms writer-stage reduction.
- patch summary: temporarily added an exact-slot writer for group slots 0-2,
  partition slot 3, segmented slot 4, sparse slots 5-6, and presence-value
  slot 7.
- commands run:
  - `cargo check`
  - `cargo test --test writer_reader_api aura0_to_aura1_default_fast_path_matches_column_fallback -- --nocapture`
  - `cargo build --release --bin aura-bench`
  - huff/no-Huffman production benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_materialized_exactslot_writer.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_materialized_exactslot_writer.json`
- result: no-Huffman exact-slot writer was 88.831 ms, slower than the C4
  repeat at 80.625 ms, with no meaningful writer-stage improvement.
- keep/reject decision: rejected and removed.
- next implication: C4 dispatch was not the remaining hard floor.

## Compact Experiment C6: Raw append without zero-fill

- hypothesis: the fixed-row writers spend about 20 ms zero-filling 36.4 MB of
  Aura1 body bytes before overwriting every byte.
- prior-art basis: fixed-width decoders should write each output byte once in
  production mode; verification scans must be explicit, not hidden in the
  allocation path.
- file/function targeted:
  - `src/generic_planner.rs::try_write_partitioned_sparse_i64_aura1_body_inner`
  - `src/generic_planner.rs::try_write_streaming_config_i64_aura1_body_from_streams`
  - `src/generic_planner.rs::write_partitioned_sparse_aura1_row_ptr`
- expected speedup: remove most of the 20-21 ms allocation/zero-fill stage.
- patch summary: for no-guard production writes, reserve the body capacity,
  write fixed rows through raw pointers while `Vec::len` is unchanged, and
  call `set_len` only after the body validates. Guarded modes keep the safe
  resized buffer path.
- commands run:
  - `cargo check`
  - `cargo test --test writer_reader_api aura0_to_aura1_default_fast_path_matches_column_fallback -- --nocapture`
  - `cargo test --test writer_reader_api aura0_to_aura1_fused_output_guard_matches_full_scan -- --nocapture`
  - `cargo build --release --bin aura-bench`
  - huff/no-Huffman production benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_materialized_nozerofill_rawappend.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_materialized_nozerofill_rawappend.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/final_huff_compact_optimized_verify.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/final_nohuff_compact_optimized_verify.json`
- result: allocation timing dropped to 0.000 ms. Best huff optimized sample was
  75.251 ms vs 81.811 ms baseline; best no-Huffman optimized sample was
  79.386 ms vs 110.089 ms baseline. Verify-mode output bytes were equal with
  hashes `12194870092346231300` and `10372430540135078667`.
- keep/reject decision: keep for no-guard compact production writes.
- next implication: remaining time is semantic stream decode plus row
  reconstruction/writes, not hidden output zero-fill.

## Compact Experiment C7: Remove per-row offset/slice construction

- hypothesis: after C6, no-guard row writes still paid checked offset and slice
  construction before using the raw pointer writer.
- prior-art basis: fixed-row output range can be validated once per block.
- file/function targeted:
  - `src/generic_planner.rs::try_write_partitioned_sparse_i64_aura1_body_inner`
  - `src/generic_planner.rs::try_write_streaming_config_i64_aura1_body_from_streams`
- expected speedup: 1-3 ms writer-stage reduction.
- patch summary: temporarily moved no-guard pointer writes ahead of safe slice
  construction.
- commands run:
  - `cargo check`
  - `cargo build --release --bin aura-bench`
  - huff/no-Huffman production benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/huff_materialized_rawappend_nooffset.json`
  - `/tmp/aura-benchmarks/compact-semantic-20260623T044831Z/nohuff_materialized_rawappend_nooffset.json`
- result: huff slowed to 76.000 ms from C6's 75.251 ms; no-Huffman slowed to
  82.814 ms from C6's 79.386 ms.
- keep/reject decision: rejected and reverted.
- next implication: the remaining writer cost is actual row reconstruction and
  byte stores, not offset arithmetic.

## Experiment 1: Fair Benchmark Truth Validation

- hypothesis: The current Aura0-vs-zstd loss is a real fair production result, not a sink or verification mismatch.
- file/function targeted:
  - `src/bin/aura_bench.rs` fair operations
  - `/tmp/aura-benchmarks/zstd-resolution-lane-5/*.json`
- expected speedup: none.
- patch summary: none; read-only validation.
- commands run: read-only source/JSON inspection by Benchmark Truth subagent.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/zstd-resolution-lane-5/huff_aura0_to_aura1_bytes.json`
  - `/tmp/aura-benchmarks/zstd-resolution-lane-5/huff_zstd_l3_aura1_bytes.json`
  - `/tmp/aura-benchmarks/zstd-resolution-lane-5/nohuff_aura0_to_aura1_bytes.json`
  - `/tmp/aura-benchmarks/zstd-resolution-lane-5/nohuff_zstd_l3_aura1_bytes.json`
- result: fair production benchmark is valid; both paths use `memory_vec`, `no_guard`, `canonical_hash_mode=none`, and no output verification. Aura0 loses by 19.839 ms on huff and 45.498 ms on nohuff. Label caveat: zstd top-level `input_bytes` is the reference Aura1 input path, while measured compressed bytes are in fair fields.
- keep/reject decision: keep the benchmark as the target; add `benchmark_input_bytes` in the prototype patch to reduce future ambiguity.
- next implication: treat the current semantic path as losing and proceed to measured speed-lane prototype work.

## Experiment 2: Existing Semantic Aura0 Decode Speed-Limit Review

- hypothesis: The current semantic layout loses because it reconstructs fields and writes fixed Aura1 rows, not because input reads dominate.
- file/function targeted:
  - `src/records.rs::try_compile_aura0_to_aura1_fast_profiled`
  - `src/generic_planner.rs::try_write_generic_i64_aura1_body_from_streams_profiled`
  - `src/body.rs` generic stream decoders
- expected speedup: none; identifies target.
- patch summary: none; read-only stage review.
- commands run: read-only source/JSON inspection by Decode Speed-Limit subagent.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/zstd-resolution-lane-5/huff_aura0_to_aura1_bytes.json`
  - `/tmp/aura-benchmarks/zstd-resolution-lane-5/nohuff_aura0_to_aura1_bytes.json`
- result: huff semantic Aura0 spends about 37.819 ms decoding streams and 41.303 ms writing Aura1; nohuff spends about 32.806 ms decoding streams and 70.334 ms writing Aura1. Both materialize 12 stream vectors and 2,754,892 stream values. No hard proof that code optimization can never win, but current evidence says a large writer/reconstruction reduction is required, especially nohuff.
- keep/reject decision: keep as evidence that the product failure is in semantic reconstruction/write work.
- next implication: implement a speed-lane prototype that avoids semantic reconstruction for byte-output expansion.

## Experiment 3: Aura0 Byte-Lane Prototype CLI Contract

- hypothesis: A benchmark-selectable Aura0-fast byte lane can be represented as a prebuilt single-block payload and expanded to Aura1 bytes with the same memory sink as zstd.
- file/function targeted:
  - `tests/aura_bench_cli.rs::aura_bench_reports_aura0_byte_lane_speed_prototype`
  - `src/bin/aura_bench.rs`
- expected speedup: raw/lz4 lane should beat external zstd because it avoids semantic reconstruction and, for lz4/raw, uses faster byte expansion.
- patch summary: added a failing CLI test, then implemented `aura0-byte-lane-to-aura1-bytes`, `aura0-byte-lane-to-aura1-bytes-verify`, `--byte-lane-codec raw|lz4|zstd1|zstd3|zstd9`, a single-block byte-lane payload header, guard validation, and JSON fields including `benchmark_input_bytes`, `aura0_profile`, `byte_lane_*`.
- commands run:
  - `cargo test --test aura_bench_cli aura_bench_reports_aura0_byte_lane_speed_prototype -- --nocapture`
- benchmark JSON paths: none; test-level fixture only.
- result: red test first failed on unknown operation; implementation then passed the targeted test.
- keep/reject decision: keep prototype for fair huff/nohuff benchmarks.
- next implication: run five concrete production benchmarks: semantic Aura0, byte-lane raw, zstd1, zstd3, lz4, plus external zstd L3 for comparison.

## Experiment 4: Byte-Lane Production Guard Scan Removal

- hypothesis: The first byte-lane prototype was slow because it validated the output guard in production mode, adding a full second scan of Aura1 bytes.
- file/function targeted:
  - `src/bin/aura_bench.rs::decode_aura1_byte_lane`
  - `src/bin/aura_bench.rs::fair_aura1_bytes_operation`
  - `tests/aura_bench_cli.rs::aura_bench_reports_aura0_byte_lane_speed_prototype`
- expected speedup: remove roughly one output-byte guard scan from production byte-lane runs.
- patch summary: added `byte_lane_guard_validated` JSON field; production byte-lane decode validates lane structure and lengths only, while `*-verify` validates the output guard and reports output equality/hash.
- commands run:
  - `cargo test --test aura_bench_cli aura_bench_reports_aura0_byte_lane_speed_prototype -- --nocapture`
  - `cargo build --release --bin aura-bench`
- benchmark JSON paths:
  - pre-fix: `/tmp/aura-benchmarks/speed-lane-20260623T023803Z/*byte_lane*.json`
  - post-fix: `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/*byte_lane*.json`
- result: huff raw dropped from 73.630 ms to 25.163 ms; huff lz4 dropped from 96.158 ms to 49.003 ms; nohuff raw dropped from 71.574 ms to 25.567 ms; nohuff lz4 dropped from 97.076 ms to 54.050 ms.
- keep/reject decision: keep. Production target must not include verification guard scans unless zstd also verifies equivalently.
- next implication: compare post-fix byte lanes to external zstd L3/L9.

## Experiment 5: Raw Aura1 Byte Lane

- hypothesis: A raw Aura1 byte lane defines the copy-only upper bound for Aura0-fast expansion.
- file/function targeted:
  - `src/bin/aura_bench.rs::build_aura1_byte_lane`
  - `src/bin/aura_bench.rs::decode_aura1_byte_lane`
- expected speedup: beat zstd by avoiding decompression and semantic reconstruction, at the cost of storing full Aura1 bytes.
- patch summary: `--byte-lane-codec raw`.
- commands run:
  - `target/release/aura-bench --operation aura0-byte-lane-to-aura1-bytes ... --byte-lane-codec raw` for huff/nohuff.
  - `target/release/aura-bench --operation aura0-byte-lane-to-aura1-bytes-verify ... --byte-lane-codec raw` for huff/nohuff.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/huff_byte_lane_raw.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/nohuff_byte_lane_raw.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/huff_byte_lane_raw_verify.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/nohuff_byte_lane_raw_verify.json`
- result: huff 25.163 ms and nohuff 25.567 ms. Both beat external zstd L3, but the lane is about 36.4 MB, larger than zstd and much larger than compact Aura0.
- keep/reject decision: keep as speed-limit prototype only; not a practical default compressed profile.
- next implication: lz4 is the practical fast compressed byte-lane candidate.

## Experiment 6: LZ4 Aura1 Byte Lane

- hypothesis: An lz4-compressed Aura1 byte lane can beat external zstd L3 while retaining compression.
- file/function targeted:
  - `src/bin/aura_bench.rs::build_aura1_byte_lane`
  - `src/bin/aura_bench.rs::decode_aura1_byte_lane`
- expected speedup: beat external zstd L3 on huff/nohuff with a larger but faster lane.
- patch summary: added direct `lz4_flex = "0.11.6"` dependency and `--byte-lane-codec lz4`.
- commands run:
  - `target/release/aura-bench --operation aura0-byte-lane-to-aura1-bytes ... --byte-lane-codec lz4` for huff/nohuff.
  - `target/release/aura-bench --operation aura0-byte-lane-to-aura1-bytes-verify ... --byte-lane-codec lz4` for huff/nohuff.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/huff_byte_lane_lz4.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/nohuff_byte_lane_lz4.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/huff_byte_lane_lz4_verify.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/nohuff_byte_lane_lz4_verify.json`
- result: huff lz4 49.003 ms vs external zstd L3 62.132 ms; nohuff lz4 54.050 ms vs external zstd L3 67.762 ms. Output equality verified for both datasets. Lane size is about 9.78 MB, larger than zstd Aura1 and much larger than compact Aura0.
- keep/reject decision: keep as the current Aura0-fast compressed speed-lane prototype.
- next implication: real format work should serialize byte-lane descriptors in the compiled footer and make lz4 an explicit fast profile option.

## Experiment 7: Zstd Aura1 Byte Lanes

- hypothesis: A zstd-compressed Aura1 byte lane should behave like external `.aura1.zst`, proving the lane wrapper does not require semantic reconstruction.
- file/function targeted:
  - `src/bin/aura_bench.rs::build_aura1_byte_lane`
  - `src/bin/aura_bench.rs::decode_aura1_byte_lane`
- expected speedup: zstd9 may beat zstd L3 because it stores fewer bytes; zstd1/zstd3 should be close to external zstd at the same level.
- patch summary: `--byte-lane-codec zstd1|zstd3|zstd9`.
- commands run:
  - `target/release/aura-bench --operation aura0-byte-lane-to-aura1-bytes ... --byte-lane-codec zstd1|zstd3|zstd9` for huff/nohuff.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/huff_byte_lane_zstd1.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/huff_byte_lane_zstd3.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/huff_byte_lane_zstd9.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/nohuff_byte_lane_zstd1.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/nohuff_byte_lane_zstd3.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/nohuff_byte_lane_zstd9.json`
- result: zstd9 byte lane passed the zstd L3 target on both datasets: huff 56.611 ms, nohuff 57.870 ms. zstd1/zstd3 were mixed or close to external zstd; they are not the preferred fast profile.
- keep/reject decision: keep zstd9 as a size-biased speed-lane option to evaluate later; reject zstd1/zstd3 as the speed default because lz4 is faster.
- next implication: final recommendation should distinguish `aura0-compact` from `aura0-fast-lz4` and `aura0-fast-zstd9`.

## Experiment 8: Existing Semantic Aura0 Recheck

- hypothesis: The existing compact semantic layout still loses after the byte-lane work; speed target is solved only by the prototype lane.
- file/function targeted:
  - `src/records.rs::try_compile_aura0_to_aura1_fast_profiled`
  - `src/generic_planner.rs` semantic stream decode/write path
- expected speedup: none.
- patch summary: no semantic decode changes in this sprint.
- commands run:
  - `target/release/aura-bench --operation aura0-to-aura1-bytes ...` for huff/nohuff.
  - `target/release/aura-bench --operation zstd-aura1-to-aura1-bytes ... --zstd-level 3` for huff/nohuff.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/huff_semantic_aura0.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/nohuff_semantic_aura0.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/huff_external_zstd_l3.json`
  - `/tmp/aura-benchmarks/speed-lane-20260623T024321Z/nohuff_external_zstd_l3.json`
- result: huff semantic Aura0 87.995 ms vs external zstd L3 62.132 ms; nohuff semantic Aura0 105.834 ms vs external zstd L3 67.762 ms.
- keep/reject decision: existing semantic Aura0 remains compact archival mode, not the zstd-beating byte-output profile.
- next implication: claim speed target only for the Aura0-fast byte-lane prototype, not for current serialized compact Aura0.

## Experiment 9: Real Footer-Serialized Aura0 Byte Lane

- hypothesis: Moving the byte lane from benchmark-only payloads into real
  Aura0 `AURP` footer/body behavior can preserve the prototype's speed target.
- prior-art reasoning: DBN/zstd/LZ4 all win hot replay/expand paths by moving
  metadata out of the hot loop and emitting bytes directly. A footer-described
  byte lane gives Aura0 the same direct byte-output lane while keeping compact
  semantic streams available.
- file/function targeted:
  - `src/program.rs::CompiledFooter::encode/decode`
  - `src/records.rs::compile_i64_file_with_aura0_profile`
  - `src/records.rs::compile_aura0_to_aura1_bytes_with_lane`
  - `src/bin/aura_bench.rs` fair byte-lane operation
- expected speedup: match prototype lz4/raw decode times while making the lane
  a real Aura0 file feature.
- implementation patch: added `AUBL` footer descriptor table, real
  compact/fast/hybrid writer APIs, real lane-selected Aura0 reader APIs, CLI
  `--aura0-profile` and `--use-byte-lane`, and tests for lz4/zstd3 round trips,
  hybrid semantic fallback, forced missing lane rejection, corrupt payload
  checksum rejection, unsupported codec rejection, and footer descriptor
  round-trip.
- commands run:
  - `cargo test --test writer_reader_api byte_lane -- --nocapture`
  - `cargo test --test footer_preservation byte_lane -- --nocapture`
  - `cargo test --test aura_bench_cli -- --nocapture`
  - `cargo test`
  - `cargo build --release --bin aura-bench`
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/real-byte-lane-20260623T031520Z/*.json`
- result: real files were correct, but first production fair benchmark was too
  slow: huff fast lz4 118.293 ms and nohuff fast lz4 116.738 ms. The real
  reader decoded into a temporary output, zero-filled a second output buffer,
  and copied the full Aura1 bytes again.
- keep/reject decision: keep real format integration; reject the first decode
  implementation as too copy-heavy.
- next implication: eliminate the extra full-output allocation/copy for the
  single-block lane.

## Experiment 10: Single-Block Direct Byte-Lane Return

- hypothesis: Returning the decoded single-block Aura1 bytes directly instead
  of copying them into a pre-zeroed output Vec should recover most of the
  prototype speed.
- prior-art reasoning: zstd/LZ4 decoders emit one contiguous byte buffer. A
  single Aura1 byte-lane block with output offset zero does not need a second
  stitching pass.
- file/function targeted:
  - `src/records.rs::decode_aura1_byte_lanes_from_body`
- expected speedup: remove one 36.4 MB zero/copy pass.
- implementation patch: added a single-descriptor fast path that validates the
  descriptor, decodes the payload, validates length/checksum when requested, and
  returns the decoded Vec directly.
- commands run:
  - `cargo test --test writer_reader_api byte_lane -- --nocapture`
  - `cargo test --test aura_bench_cli aura_bench_reports_real_aura0_byte_lane_profile -- --nocapture`
  - `cargo build --release --bin aura-bench`
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/real-byte-lane-direct-20260623T031701Z/*.json`
- result: improved but still lost: huff fast lz4 94.231 ms, nohuff fast lz4
  94.480 ms, external zstd L3 around 64/62 ms.
- keep/reject decision: keep; it is correct and removes avoidable copying.
- next implication: locate the remaining fixed cost.

## Experiment 11: Footer-Tail Descriptor Parser

- hypothesis: The remaining gap is full `CompiledFooter` decode before finding
  byte lanes; parse only the trailing `AUBL` extension for byte-output expansion.
- prior-art reasoning: footer metadata should be parsed once and minimally for
  the selected operation. Byte-output expansion does not need schema, generic
  semantic plans, or field programs.
- file/function targeted:
  - `src/records.rs::try_decode_aura1_byte_lane_from_footer_tail`
  - `src/records.rs::try_compile_aura0_to_aura1_fast`
  - `src/records.rs::try_compile_aura0_to_aura1_fast_guarded`
  - `src/records.rs::try_compile_aura0_to_aura1_fast_profiled`
- expected speedup: remove full semantic footer/program decode from byte-lane
  expansion.
- implementation patch: added a tail parser that locates `AUBL` inside footer
  bytes, decodes only lane descriptors, and dispatches raw/lz4/zstd payload
  expansion before full `CompiledFooter::decode`.
- commands run:
  - `cargo test --test writer_reader_api byte_lane -- --nocapture`
  - `cargo test --test aura_bench_cli aura_bench_reports_real_aura0_byte_lane_profile -- --nocapture`
  - `cargo build --release --bin aura-bench`
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/real-byte-lane-tail-20260623T032142Z/*.json`
  - `/tmp/aura-benchmarks/real-byte-lane-tail-20260623T032142Z/huff_transcode_real_fast_lz4.json`
- result: normal production transcode on a preserved fast lz4 Aura0 file ran
  in 30.991 ms, proving the real reader tail path was fast. The fair byte-lane
  benchmark still reported about 92 ms because it was using the slower public
  convenience wrapper instead of the production profiled reader.
- keep/reject decision: keep tail parser; fix benchmark routing.
- next implication: make fair byte-lane operation use the same production reader
  path as real transcode.

## Experiment 12: Fair Benchmark Uses Production Reader Path

- hypothesis: The fair byte-lane operation must call the production profiled
  Aura0 reader path; otherwise it measures a convenience wrapper rather than
  the product path.
- prior-art reasoning: benchmark truth requires identical work and sink, and no
  accidental wrapper work around one side of the comparison.
- file/function targeted:
  - `src/bin/aura_bench.rs::fair_aura1_bytes_operation`
- expected speedup: match the preserved-file transcode result for fast/hybrid
  byte lanes.
- implementation patch: rerouted real byte-lane fair operations through
  `records::try_compile_i64_file_profiled(... Profile::Aura1 ...)` for
  `--use-byte-lane auto|always`, preserving `--use-byte-lane never` as semantic
  fallback. The operation now times the same reader path used by
  `transcode-aura0-to-aura1`.
- commands run:
  - `cargo test --test aura_bench_cli aura_bench_reports_real_aura0_byte_lane_profile -- --nocapture`
  - `cargo test --test writer_reader_api byte_lane -- --nocapture`
  - `cargo build --release --bin aura-bench`
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/*.json`
- result: target passed. Huff fast lz4 44.373 ms vs zstd L3 61.386 ms; nohuff
  fast lz4 43.281 ms vs zstd L3 62.798 ms. Hybrid lz4 also passed: huff
  43.761 ms, nohuff 44.058 ms. Fast raw gave the copy-only limit: huff
  24.753 ms, nohuff 25.271 ms.
- keep/reject decision: keep. This is the accepted working implementation path.
- next implication: document hybrid/lz4 as the final speed-profile candidate and
  keep compact semantic Aura0 as archival/canonical mode.

## Experiment 13: Real Byte-Lane Verification

- hypothesis: The speed-lane win is only acceptable if output equality and
  byte-lane guard validation still pass.
- prior-art reasoning: production timing can skip verification, but strict
  verification must prove the output bytes match the Aura1 reference.
- file/function targeted:
  - `src/records.rs::validate_aura1_byte_lane_checksum`
  - `src/bin/aura_bench.rs` `aura0-byte-lane-to-aura1-bytes-verify`
- expected speedup: none; correctness gate.
- implementation patch: verify operation uses strict guard mode in the
  production reader path and reports output equality/hash.
- commands run:
  - `target/release/aura-bench --operation aura0-byte-lane-to-aura1-bytes-verify ... --aura0-profile fast --byte-lane-codec lz4 --use-byte-lane always`
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/huff_fast_lz4_verify.json`
  - `/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/nohuff_fast_lz4_verify.json`
- result: `output_bytes_equal=true`. Output byte hashes were
  `12194870092346231300` for huff and `10372430540135078667` for nohuff.
- keep/reject decision: keep verification mode; keep production no_guard timing
  separate from strict verification.
- next implication: final status should be `PASSED_WITH_HYBRID_PROFILE`, with
  `fast` as byte-lane-only and `compact` as smallest archival profile.

## Compact Deep Experiment C8: Zstd opponent warm30 truth table

- hypothesis: The compact target must be compared against fresh zstd L3
  warm30 results with the same memory sink and no production verification.
- prior-art basis: zstd emits already-formed bytes and avoids semantic field
  reconstruction; benchmark noise must not decide the target.
- file/function targeted:
  - `src/bin/aura_bench.rs` fair zstd operations.
- expected speedup: none; establishes opponent.
- patch summary: no code patch.
- commands run:
  - `target/release/aura-bench --operation zstd-aura1-to-aura1-bytes ... --zstd-level 1`
  - `target/release/aura-bench --operation zstd-aura1-to-aura1-bytes ... --zstd-level 3`
  - `target/release/aura-bench --operation zstd-aura1-to-aura1-bytes ... --zstd-level 9`
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final_huff_zstd_l1_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final2_huff_zstd_l3_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final_huff_zstd_l9_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final_nohuff_zstd_l1_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final2_nohuff_zstd_l3_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final_nohuff_zstd_l9_warm30.json`
- result: zstd L3 target was 62.988 ms huff and 62.366 ms nohuff. Zstd L9
  was faster and smaller than L3 on both fixtures.
- keep/reject decision: keep as opponent evidence.
- next implication: compact Aura0 must beat roughly 63 ms on both datasets to
  pass the stated L3 target.

## Compact Deep Experiment C9: Prechecked fixed-row stores

- hypothesis: fixed-row stores spend avoidable time narrowing i64 values inside
  every row write.
- prior-art basis: DBN-style fixed-width replay should validate transforms once
  near the source and write stable field widths directly.
- file/function targeted:
  - `src/generic_planner.rs::write_partitioned_sparse_aura1_row_ptr`
  - `src/generic_planner.rs::try_write_partitioned_sparse_i64_aura1_body_inner`
- expected speedup: 2-5 ms writer-stage reduction.
- patch summary: added `write_partitioned_sparse_aura1_row_ptr_prechecked`
  and preconverted `i32`/`i8` row values before the raw pointer store in the
  fixed Aura1 layout path.
- commands run:
  - `cargo build --release --bin aura-bench`
  - huff/nohuff compact production benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/huff_prechecked_row_store.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/nohuff_prechecked_row_store.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/huff_prechecked_row_store_repeat.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/nohuff_prechecked_row_store_repeat.json`
- result: huff improved to 62.560-63.611 ms in repeat samples; nohuff improved
  to 80.024-81.100 ms but still lost to zstd.
- keep/reject decision: keep.
- next implication: row store conversions were avoidable but not the full gap.

## Compact Deep Experiment C10: Dense partition base map

- hypothesis: per-run partition base lookup should be dense table lookup, not
  binary search or linear lookup, because the fixed Aura1 partition field is i8.
- prior-art basis: footer/plan decode should turn stream IDs and small-domain
  keys into direct lookup tables before hot loops.
- file/function targeted:
  - `src/generic_planner.rs::try_write_partitioned_sparse_i64_aura1_body_inner`
  - `src/generic_planner.rs::try_write_streaming_config_i64_aura1_body_from_streams`
- expected speedup: 1-3 ms on huff and reduce no-Huffman writer overhead.
- patch summary: added dense `[Option<i64>; 256]` partition base tables for
  i8 partition-key paths while preserving fallback linear lookup for generic
  non-i8 shapes.
- commands run:
  - `cargo build --release --bin aura-bench`
  - huff/nohuff compact production benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/huff_dense_base_map.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/nohuff_dense_base_map.json`
- result: huff sample improved to 60.857 ms and nohuff to 76.116 ms, but later
  warm30 showed the stable kept path still loses to zstd L3.
- keep/reject decision: keep dense map where type-compatible.
- next implication: single-run pass is not enough; warm30 must decide.

## Compact Deep Experiment C11: Cursor-direct recheck

- hypothesis: after writer improvements, cursor-direct might become viable by
  removing full stream vector materialization.
- prior-art basis: direct cursor-to-row execution should avoid full-file
  stream `Vec<i64>` materialization.
- file/function targeted:
  - existing `--decode-path cursor` route in `src/records.rs` and
    `src/generic_planner.rs`.
- expected speedup: remove materialized stream vectors.
- patch summary: no new patch; rebenchmarked existing cursor path.
- commands run:
  - `target/release/aura-bench --operation aura0-to-aura1-bytes ... --decode-path cursor`
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/huff_cursor_dense_base.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/nohuff_cursor_dense_base.json`
- result: huff cursor was 122.180 ms and nohuff cursor was 104.923 ms despite
  `direct_cursor_stream_count=12` and zero materialized streams.
- keep/reject decision: reject as default; keep behind flag for experiments.
- next implication: eliminating materialization in the current cursor executor
  is not enough because cursor row execution is slower.

## Compact Deep Experiment C12: Pointer-advance row loop

- hypothesis: advancing a row pointer linearly avoids repeated
  `row * ROW_WIDTH` address calculations.
- prior-art basis: fixed-width scans commonly advance pointers rather than
  recomputing offsets.
- file/function targeted:
  - `src/generic_planner.rs` no-guard fixed row loops.
- expected speedup: 0.5-2 ms.
- patch summary: temporary pointer-advance row loop.
- commands run:
  - `cargo build --release --bin aura-bench`
  - huff/nohuff compact production benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/huff_pointer_advance.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/nohuff_pointer_advance.json`
- result: huff slowed to 68.154 ms and nohuff slowed to 87.763 ms.
- keep/reject decision: reverted.
- next implication: the compiler already handles the indexed pointer pattern
  well enough; this is not the bottleneck.

## Compact Deep Experiment C13: Exact no-Huffman streaming writer

- hypothesis: no-Huffman loses because the materialized streaming writer still
  executes generic slot loops despite the fixture's fixed 8-field layout.
- prior-art basis: generated writer recipes should execute direct field writes
  when schema, slots, and guard mode match.
- file/function targeted:
  - `src/generic_planner.rs::try_write_streaming_config_i64_aura1_body_from_streams`
- expected speedup: close most of no-Huffman's writer gap.
- patch summary: added an exact no-guard/no-Huffman branch for group slots
  0-2, partition slot 3, segmented slot 4, sparse slots 5-6, and presence
  value slot 7.
- commands run:
  - `cargo build --release --bin aura-bench`
  - huff/nohuff compact production benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/huff_exact_streaming_branch.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/nohuff_exact_streaming_branch.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/huff_exact_nohuff_gate.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/nohuff_exact_nohuff_gate.json`
- result: ungated exact branch hurt huff but helped nohuff. Gating it to
  non-Huffman plans produced a 65.830 ms nohuff sample.
- keep/reject decision: keep only behind the no-Huffman/shape guard.
- next implication: specialized row recipes help, but not enough for a stable
  target pass.

## Compact Deep Experiment C14: Delta accumulator codec rewrite

- hypothesis: `values.last()` inside delta decode loops is extra work that can
  be replaced by a local accumulator.
- prior-art basis: integer decode loops should keep prefix-sum state in a
  scalar register.
- file/function targeted:
  - `src/body.rs` previous-delta and previous-varint decode loops.
- expected speedup: reduce decode stage by 1-3 ms.
- patch summary: temporary accumulator rewrite.
- commands run:
  - `cargo build --release --bin aura-bench`
  - huff/nohuff compact production benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final_huff_delta_accumulator_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final_nohuff_delta_accumulator_warm30.json`
- result: huff was 65.551 ms and nohuff 66.129 ms, with p95 regression versus
  the best kept candidate.
- keep/reject decision: reverted.
- next implication: scalar codec micro-edits do not overcome stream-to-row
  reconstruction costs.

## Compact Deep Experiment C15: Sparse prevalidation

- hypothesis: prevalidating presence masks allows unchecked sparse stream reads
  inside the row loop.
- prior-art basis: one block-level validation can replace repeated bounds
  checks when stream cardinalities are known.
- file/function targeted:
  - `src/generic_planner.rs` no-Huffman exact writer branch.
- expected speedup: reduce sparse branch/index overhead.
- patch summary: temporary mask pre-scan and unchecked sparse indexing.
- commands run:
  - `cargo build --release --bin aura-bench`
  - huff/nohuff compact production benchmarks below.
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final_huff_sparse_prevalidated_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-20260623T193509Z/final_nohuff_sparse_prevalidated_warm30.json`
- result: huff slowed to 67.576 ms and nohuff slowed to 69.963 ms.
- keep/reject decision: reverted.
- next implication: extra validation passes are not acceptable in production
  timing unless they also eliminate more row work.

## Compact Deep Experiment C16: Final kept compact warm30 verification

- hypothesis: the kept compact-only optimizations improve the path but still
  need a warm30 fair comparison against zstd L3.
- prior-art basis: no speed claim is valid without before/after benchmark
  numbers and p95.
- file/function targeted:
  - `src/generic_planner.rs` fixed-row compact writer.
- expected speedup: retain huff/nohuff improvements without p95 instability.
- patch summary: kept prechecked fixed row stores, dense partition-base maps,
  and the gated no-Huffman exact writer; reverted pointer advance, delta
  accumulator, and sparse prevalidation.
- commands run:
  - `target/release/aura-bench --operation aura0-to-aura1-bytes ...`
  - `target/release/aura-bench --operation aura0-to-aura1-bytes-verify ...`
  - `target/release/aura-bench --operation zstd-aura1-to-aura1-bytes ... --zstd-level 3`
- benchmark JSON paths:
  - `/tmp/aura-benchmarks/compact-research-final-ba3cc23/huff_compact_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-final-ba3cc23/nohuff_compact_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-final-ba3cc23/huff_compact_verify.json`
  - `/tmp/aura-benchmarks/compact-research-final-ba3cc23/nohuff_compact_verify.json`
  - `/tmp/aura-benchmarks/compact-research-final-ba3cc23/huff_zstd_l3_warm30.json`
  - `/tmp/aura-benchmarks/compact-research-final-ba3cc23/nohuff_zstd_l3_warm30.json`
- result: post-commit compact huff 64.569 ms vs zstd L3 62.425 ms; compact
  nohuff 69.679 ms vs zstd L3 63.164 ms. Verify outputs matched Aura1
  references.
- keep/reject decision: keep the compact code improvements, but mark the speed
  target failed for the current compact layout.
- next implication: a compact v2 semantic stream layout is required for another
  serious attempt; byte lanes are not a valid answer for this compact sprint.

## SDK Lane S1: Public dynamic-schema facade

- hypothesis: Aura needs a reusable library surface before further format work
  is useful to non-benchmark callers.
- prior-art basis: columnar formats expose schema, writer, reader, and
  conversion APIs independently of their benchmark harnesses.
- file/function targeted:
  - `src/schema.rs` dynamic SDK schema facade.
  - `src/types.rs` SDK record batch/value facade.
  - `src/writer.rs` public writer facade.
  - `src/reader.rs` public reader facade.
  - `src/convert.rs` public conversion helper.
  - `src/options.rs` public format/options types.
- expected speedup: none. This is SDK surface area, not a hot-path
  optimization.
- patch summary: added schema-generic fixed-width scalar API over the existing
  generic i64 engine, plus examples, docs, and non-grimoire roundtrip tests.
- commands run:
  - `cargo check`
  - `cargo check --examples`
  - `cargo test --test sdk_api -- --nocapture`
  - full verification commands are recorded in the final SDK report.
- benchmark JSON paths: none for this SDK lane.
- result: pending full verification at time of entry.
- keep/reject decision: keep if full SDK tests and examples pass.
- next implication: typed column batches, generic benchmarks, and full plan API
  exposure remain separate milestones.

## SDK Lane S2: Plan, column batches, streaming-shaped reader, and metadata

- hypothesis: the SDK facade becomes materially usable only when users can
  inspect the compiled plan, write typed column batches, iterate batches, and
  preserve schema display metadata through formats.
- prior-art basis: Parquet-like APIs expose schema/metadata separately from the
  benchmark harness and prefer typed column batches for bulk writes.
- file/function targeted:
  - `src/program.rs` public `CompiledAuraPlan` from SDK schema.
  - `src/types.rs` `AuraColumnBatch` and batch trait.
  - `src/writer.rs` plan exposure and generic batch writes.
  - `src/reader.rs` plan exposure, batch iteration, and replay visitor.
  - `src/schema.rs` named schema block encoding.
- expected speedup: none. This removes SDK blockers, not hot-path benchmark
  bottlenecks.
- patch summary: exposed schema-derived plan metadata, added typed column
  batches, added reader batch iteration/replay APIs, and preserved schema name
  plus schema ID in schema blocks.
- commands run:
  - `cargo check`
  - `cargo check --examples`
  - `cargo test`
  - `cargo build --release --bin aura-bench`
- benchmark JSON paths: none for this API lane.
- result: committed as `9e5ffda Add SDK plan column and streaming APIs`.
- keep/reject decision: keep.
- next implication: true block-streaming reader remains an implementation gap
  because `AuraReader` still decodes through the row engine internally.

## SDK Lane S3: Generated SDK fixture benchmark smoke matrix

- hypothesis: generic benchmark proof must include non-grimoire generated
  schemas, even if the full performance matrix remains a longer-running local
  artifact.
- prior-art basis: generic format claims require schema-shape variation:
  narrow, wide, reordered, dense, sparse, tiny, and edge-case fixtures.
- file/function targeted:
  - `src/bin/aura_fixture_gen.rs`
  - `tests/fixture_generation.rs`
  - `tests/aura_bench_cli.rs`
- expected speedup: none.
- patch summary: generated SDK fixture families with schema metadata and
  emitted `sdk_bench_smoke.json`; added tests that run one SDK fixture through
  the benchmark CLI.
- commands run:
  - `cargo test`
  - `cargo build --release --bin aura-bench`
- benchmark JSON paths: generated under temporary test directories and fresh
  local verification directories.
- result: committed as `2755e62 Add SDK generic fixture benchmark matrix`.
- keep/reject decision: keep.
- next implication: expand smoke entries into repeated warm performance sweeps
  before publishing SDK performance claims.
