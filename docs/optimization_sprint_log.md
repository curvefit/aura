# AURA0 Speed Target Sprint Log

Current target: make `.aura0 -> .aura1` byte expansion beat `.aura1.zst -> .aura1`
byte expansion for both `grimoire-50mb-huff` and `grimoire-50mb-nohuff`, or
prove the semantic layout needs a format-level speed lane and implement the
smallest working prototype that wins.

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
