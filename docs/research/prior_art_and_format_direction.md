# Prior Art And Aura0 Format Direction

Status: RESEARCH COMPLETE: BYTE LANE SHOULD BE REAL FORMAT FEATURE

## Scope

This memo was written before additional production code changes in the deep
resolution sprint. It combines external prior art with the current AURA source
and benchmark evidence.

Sources inspected:

- Databento Binary Encoding documentation:
  https://databento.com/docs/standards-and-conventions/databento-binary-encoding
- Zstandard compression format:
  https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md
- LZ4 frame format:
  https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md
- LZ4 block format:
  https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md
- Local AURA docs and code:
  - `docs/FORMAT.md`
  - `docs/FOOTER.md`
  - `docs/BENCHMARKING.md`
  - `src/bin/aura_bench.rs`
  - `src/records.rs`
  - `src/program.rs`
  - `src/generic_planner.rs`

## DBN / DBN-like Replay Lessons

DBN puts metadata first, then records. Its public format docs describe metadata
as the beginning of every stream or file, followed immediately by records. Every
record starts with a fixed common header containing length, record type,
publisher, instrument, and timestamp fields. The lesson for AURA is not that
every record must have DBN's exact header, but that replay speed comes from
doing metadata/schema interpretation before the hot scan and then walking record
bytes sequentially.

Implications for Aura1:

- Aura1 should remain the DBN-like replay layer.
- Aura1 replay should parse metadata once, then scan fixed-width rows.
- Hot replay must avoid per-record schema lookup, symbol string conversion,
  row allocation, and dynamic dispatch.
- Aura0 byte-output expansion should target already-formed Aura1 bytes when the
  caller asks for `.aura0 -> .aura1` bytes. Rebuilding fields one stream at a
  time is different work from DBN-like replay.

## Zstd Lessons

Zstd is a byte compressor with frames, blocks, literals, sequences, entropy
coding, optional dictionaries, and optional frame content size. Its decoder is
optimized to produce output bytes directly from compressed bytes. It does not
understand market-data fields and does not reconstruct semantic columns before
emitting output.

Observed AURA evidence:

- External `.aura1.zst -> .aura1` L3 expands about 36.4 MB in roughly 62-68 ms
  on the grimoire huff/nohuff fixtures.
- Compact semantic `.aura0 -> .aura1` is smaller on disk but slower because it
  decodes semantic streams, reconstructs dictionary/delta values, handles sparse
  partitions, and writes fixed Aura1 rows.
- Zstd level affects size and decode behavior. In the current benchmark, zstd9
  is smaller and decodes faster than zstd3 on the tested fixtures, likely because
  fewer compressed bytes are read and the output workload is the same.

Implications for Aura0:

- Zstd-like byte lanes are fair for a cold byte-output profile because they
  perform the same kind of work as `.aura1.zst -> .aura1`: inflate bytes.
- Zstd dictionaries may help many small blocks, but the current target is
  dominated by expanding a large Aura1 body, not by many small independent files.
- A zstd byte lane is a size-biased fast profile candidate, especially zstd9,
  but lz4 is faster in the current measurements.

## LZ4 Lessons

LZ4 block format is byte-oriented LZ77 without an entropy-coding backend or
framing layer; the framing layer is intended to be supplied externally. The LZ4
frame format has an explicit block independence flag; independent blocks can be
decoded separately, while dependent blocks require previous history. The block
format docs also emphasize safe decoding and bounds checking for corrupted
input.

Observed AURA evidence:

- The real Aura0 byte lane uses `lz4_flex::compress_prepend_size`, which is a
  block-style payload with a size prefix, not a full LZ4 frame.
- The real lz4 byte lane beats external zstd L3 on both grimoire fixtures:
  huff fast lz4 44.373 ms vs zstd L3 61.386 ms, nohuff fast lz4 43.281 ms
  vs zstd L3 62.798 ms.
- The tradeoff is size: about 9.78 MB, larger than the 5.54 MB zstd Aura1 lane
  and much larger than 1.88-2.25 MB compact semantic Aura0.

Implications for Aura0:

- Aura0-fast should use independent byte-lane blocks. The AURA footer should
  provide the framing metadata instead of relying on full LZ4 frames.
- LZ4 is the current default fast codec candidate because it wins the target
  speed test while still compressing substantially below raw Aura1.
- Block size should be benchmarked. Smaller blocks improve random access and
  independent decode but can hurt compression ratio and throughput. The first
  real implementation uses one whole-file block and serializes the block
  descriptor in the `AUBL` footer extension.

## Existing Aura0 Semantic Layout

Compact semantic Aura0 is doing more work than byte inflation:

- decode compact stream bodies
- reconstruct dictionary/delta/integer streams
- apply sparse/presence layout
- pack fixed Aura1 rows
- maintain footer/schema compatibility

This is valuable because it makes Aura0 much smaller and preserves canonical
semantic decode. It is not the same workload as inflating already-formed Aura1
bytes. It should remain a compact archival/canonical lane, not the default lane
for fastest `.aura0 -> .aura1` byte expansion.

Rejected idea:

- Claim compact semantic Aura0 is the fast profile after more minor tuning.
  Current evidence does not support this. Huff loses by about 26 ms in the fresh
  post-prototype run; nohuff loses by about 38 ms. A code-only win may still be
  possible, but it is not the direct path to the product target.

## Format Design Alternatives

### Alternative A: Keep Aura0 Compact-Only

Description:

- Aura0 contains only semantic stream lanes.
- `.aura0 -> .aura1` always reconstructs semantic streams and writes rows.

Pros:

- Smallest files.
- Strong canonical/semantic representation.
- Existing format mostly preserved.

Cons:

- Fails the zstd-speed byte-output target today.
- Makes byte-output expansion pay field reconstruction cost even when caller
  only wants Aura1 bytes.

Decision:

- Keep as `aura0-compact`, but do not use it to claim faster-than-zstd byte
  expansion.

### Alternative B: Aura0 Profiles With Optional Byte Lane

Description:

- `aura0-compact`: semantic stream lane only.
- `aura0-fast`: Aura1 byte lane as primary byte-output lane.
- `aura0-hybrid`: semantic lane plus Aura1 byte lane.

Pros:

- Compact and fast goals stop fighting each other.
- Reader can choose lane based on operation:
  - byte-output expansion uses byte lane when present;
  - canonical verification or semantic decode can use semantic lane.
- Hybrid can verify byte-lane output against semantic decode in strict mode.

Cons:

- Fast/hybrid files are larger.
- Footer layout must be extended and versioned.
- Writer and reader need explicit lane-selection rules.

Decision:

- Implement this path.

### Alternative C: Replace Aura0 Semantic Body With Zstd/LZ4 Aura1 Body

Description:

- Aura0 becomes mostly compressed Aura1 bytes plus AURA metadata.

Pros:

- Simple, fast byte expansion.

Cons:

- Loses compact semantic stream benefits.
- Does not provide a distinct semantic/canonical cold representation.
- Collapses Aura0 and Aura1.zst into nearly the same product.

Decision:

- Reject as the only format. Keep byte lane as a profile/lane, not as a full
  replacement for semantic Aura0.

## Recommended Footer Metadata

Add a byte-lane descriptor table to the compiled footer. Minimum descriptor:

```text
lane_version: u8
codec_id: u8              // raw, lz4, zstd
codec_level: i8           // 0 for raw/lz4, zstd level for zstd
block_index: u32
row_start: u64
row_count: u32
aura1_output_offset: u64
uncompressed_len: u64
compressed_offset: u64
compressed_len: u64
checksum_kind: u8         // output-byte guard initially
checksum: u64
flags: u32
```

Reader policy:

- `use-byte-lane=auto`: use byte lane for Aura0-to-Aura1 bytes when present.
- `use-byte-lane=never`: force semantic decode.
- `use-byte-lane=always`: fail clearly if byte lane is missing.
- Verify mode validates output equality/hash/checksum; production mode does not
  include a full guard scan unless requested.

Writer policy:

- compact: semantic lane only.
- fast: byte lane only where the format can still carry enough footer metadata
  to reconstruct/validate Aura1 output; otherwise fast may include a minimal
  semantic plan but byte lane is the primary expansion path.
- hybrid: semantic lane plus byte lane.

## Final Recommendation Before Coding

Make the byte lane a real Aura0 file feature and benchmark real files. Do not
spend this sprint trying to make compact semantic Aura0 beat zstd unless the
real file byte lane fails. The evidence says compact semantic Aura0 is the
archival/canonical profile, while Aura0-fast/hybrid is the speed profile.
