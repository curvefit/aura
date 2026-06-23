# Code Archaeology: Aura0 Byte-Lane Integration Points

Status: RESEARCH COMPLETE; IMPLEMENTATION LANDED AFTER THIS ARCHAEOLOGY

## Conclusion

This note originally found that the byte lane was only benchmark payload code,
not real Aura0 format code. That was correct at the time of the archaeology.
The recommended integration path below has since been implemented: `AURP`
footers can carry an `AUBL` byte-lane descriptor table, and `records.rs` can
write/read compact, fast, and hybrid Aura0 profiles.

The smallest real integration path was:

1. Add byte-lane descriptors to `CompiledFooter`.
2. Add real Aura0 writer behavior in `records.rs` for compact/fast/hybrid.
3. Add Aura0 reader/transcode lane selection before compact semantic decode.
4. Keep old compact files decoding via the semantic path when byte descriptors
   are absent.

## Current Compact Aura0 Write Path

- `src/records.rs::compile_i64_file_inner` is the normal compile implementation.
- In the `Profile::Aura0` branch, the preferred compact path uses
  `compiled_footer.generic_aura0_plan`, `encode_generic_i64_rows_with_plan`, and
  `encode_generic_i64_rows_body`.
- The fallback compact path uses the legacy `Aura0Plan` and `encode_aura0_body`.
- `encode_compiled_file` assembles header, body, footer, footer length, and seal.

## Current Compact Aura0 Read Path

- `decode_i64_file` delegates to the internal reader.
- The Aura0 branch decodes `CompiledFooter`, validates schema, then chooses the
  generic compact plan or legacy Aura0 plan.
- Generic compact decode calls `decode_generic_i64_rows_body`.
- Legacy fallback calls `decode_aura0_body`.

## Historical Byte-Lane Prototype

The old byte-lane prototype lived only in `src/bin/aura_bench.rs`:

- private byte-lane magic/version/header length
- `ByteLaneCodec`
- `ByteLanePayload`
- `build_aura1_byte_lane`
- `decode_aura1_byte_lane`
- benchmark operations `aura0-byte-lane-to-aura1-bytes` and verify variant
- fair benchmark stats fields such as `aura0-fast-byte-lane-prototype`

The current implementation no longer times this generated payload for the
product byte-lane operation. It builds a real Aura0 file outside the timed loop
and expands it through the production Aura0 reader.

## Actual Aura0 File Format Today

Actual Aura0 metadata is in `CompiledFooter`:

- schema
- compression descriptor
- record count
- block capacity
- Aura0 and Aura1 decode programs
- optional generic Aura0 plan
- chunk descriptors

`CompiledFooter` now has a production byte-lane descriptor table serialized as
the optional `AUBL` extension after chunk descriptors.

## Footer And Plan Construction

- Footer length is read from the trailer, then `CompiledFooter::decode` parses
  the `AURP` footer.
- `CompiledFooter::decode` currently finishes strictly, so unknown trailing
  footer fields are rejected.
- `CompiledAuraPlan::from_footer` is the shared plan constructor used by direct
  Aura1-to-Aura0, Aura1 layout/replay, and profiled Aura0-to-Aura1 paths.

## Where Byte-Lane Descriptors Should Live

The serialized metadata belongs in `src/program.rs::CompiledFooter`, next to
chunks. The minimal descriptor needs:

```text
lane_version
codec_id
codec_level
block_index
row_start
row_count
aura1_output_offset
uncompressed_len
compressed_offset
compressed_len
checksum_kind
checksum
flags
```

## Reader Choice Point

The Aura0 reader should choose compact vs byte lane in these places:

- `decode_i64_file_inner` Aura0 branch for row decode / validation behavior.
- profiled Aura0-to-Aura1 path before generic compact stream decode.
- any fast byte-output benchmark operation that calls production records APIs.

Lane selection policy:

- `auto`: use byte lane for Aura1 byte output when descriptors exist.
- `always`: require byte lane and fail clearly when missing.
- `never`: force compact semantic decode.

## Writer Choice Point

The writer should emit compact/fast/hybrid in:

- `compile_i64_file_inner` target-profile body selection.
- direct Aura1-to-Aura0 profiled writer for benchmarkable conversion.
- final compiled-file assembly functions.

## Exact Files To Modify

- `src/program.rs`: serialized descriptor structs, footer encode/decode, plan
  exposure.
- `src/records.rs`: real byte-lane body writer/reader, Aura0 profile selection,
  byte-lane decode branch.
- `src/bin/aura_bench.rs`: route benchmark operations through real file APIs
  instead of benchmark-only payloads.
- `tests/footer_preservation.rs`: descriptor round-trip/preservation tests.
- `tests/aura_bench_cli.rs` and/or `tests/writer_reader_api.rs`: real file
  encode/decode/equality/corruption tests.

## Unknowns

- Whether byte lane should be an authoritative fast profile body, an
  acceleration cache, or a hybrid sidecar.
- Whether old binaries must read new byte-lane files. New binaries can read old
  compact files if descriptor absence defaults to compact.
- Final block size and per-block codec policy.

## Implemented Action

The minimal real single-block byte-lane descriptor is implemented in
`CompiledFooter`, with raw/lz4/zstd byte-lane Aura0 files in `records.rs`.
Compact Aura0 remains the old-file-compatible semantic lane.
