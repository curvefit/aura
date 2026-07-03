# Adversarial Verifier: Real Aura0 Byte Lane

Status: DONE AFTER DOC FIXES

## Scope

Read-only verifier lane inspected the current patch for whether the byte lane is
still benchmark-only or has become real Aura0 file/footer behavior.

## Evidence

- `src/program.rs` defines `Aura1ByteLaneDescriptor` and stores
  `aura1_byte_lanes` in `CompiledFooter`.
- `src/program.rs` encodes and decodes the optional `AUBL` byte-lane descriptor
  table in compiled `AURP` footers.
- `src/records.rs` exposes
  `compile_i64_file_with_aura0_profile(bytes, profile, byte_lane_codec)` for
  compact, fast, and hybrid Aura0 files.
- `src/records.rs` exposes
  `compile_aura0_to_aura1_bytes_with_lane(bytes, use_byte_lane, verify)` and
  routes profiled Aura0-to-Aura1 expansion through the byte-lane path before
  semantic decode when allowed.
- `src/bin/aura_bench.rs` builds real fast/hybrid Aura0 files outside timed
  iterations and then times the production records reader path.
- `tests/writer_reader_api.rs` covers fast lz4/zstd3 round-trip, hybrid semantic
  fallback, forced byte-lane failure on compact files, corrupt payload rejection,
  and unsupported codec rejection.
- `tests/footer_preservation.rs` covers descriptor preservation.

## Initial Findings

The verifier initially marked the lane FAILED because several research documents
still described the byte lane as benchmark-only. Those documents have been
updated to distinguish historical prototype findings from the current real
`AUBL` footer implementation.

## Remaining Risks

- Old readers that strictly decode compiled footers may reject new files with
  the `AUBL` extension. New readers still accept old compact files.
- The first real byte lane uses a single whole-file block. Per-block tuning is
  still open.
- Production mode validates descriptor structure and lengths, while output-byte
  guard validation remains verify/strict work.
