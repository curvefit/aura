# Footer stamp

The Aura footer should be a stamped decode manifest, not planning evidence.
During ingest the writer may collect stats, test candidates, and choose a
layout. At stamp time it writes the final footer once. After that the footer is
constant unless the file is restamped.

## Current vs target

The current Rust container uses this trailer:

```text
footer_len  u32 little-endian
sealed:)
```

The current `.aura` ingest footer stores schema, ingest stats, Aura0 physical
plan, Aura1 physical plan, generic Aura0 instruction plan, and chunks. The
current `.aura0`/`.aura1` compiled footer stores both decode programs plus the
generic Aura0 instruction plan. Compiled-profile hotswaps copy the compiled
footer unchanged.

The slot-tail layout below is the target fast-path footer design. It is not yet
the emitted Phase 3 trailer.

The target stamped trailer locates both the whole footer and the hot slot tail:

```text
footer_len     u32 little-endian
slot_tail_len  u16 little-endian
reserved       u16
sealed:)
```

The trailing `sealed:)` bytes are the file magic for the stamped end state, so
the footer does not need a second magic string. The header already carries the
file `version` and `profile`, so the footer should not repeat the encoding type.
`slot_tail_len` points to the byte-aligned slot tail immediately before the
trailer, so an Aura1 reader can reach scales and hot widths with one backwards
jump from EOF after checking the seal.
It is part of `footer_len`, so `slot_tail_len <= footer_len`.

```text
trailer_start   = file_len - 16
footer_start    = trailer_start - footer_len
slot_tail_start = trailer_start - slot_tail_len
```

## Core fields

The stamped footer is byte-aligned metadata. Do not bitpack header or footer
fields; bitpacking belongs only in body streams. The footer is laid out so the
slot tail is last:

```text
footer body:
  group_table
  stream_table
  aux_table

slot tail:
  record_count
  slot_count
  slot_entry_size
  flags
  reserved
  slot_table
```

`record_count` is the number of logical events in the body. `slot_count` is the
number of logical data slots after interpreting the front schema map. It does
not have to equal `schema_len`, because schema control bytes such as `200` and
`201-239` describe grouping structure rather than standalone data slots. Slot
order is the logical field identity; field names live in the header comment or
the external stream dictionary, not in the footer.

The slot tail is the Aura1 fast path:

```text
record_count     u64
slot_count       u8
slot_entry_size  u8  usually 3
flags            u8
reserved         u8
slot_table       slot_count fixed entries
```

Each slot entry is three bytes:

```text
scale        i8  decimal scale used to normalize this slot
decode_kind  u8  fixed_width | fixed_step | stream | derived | group_member
decode_arg   u8  width code, stream id, group id, or aux id
```

For hot `.aura1` fixed-width slots, `decode_arg` uses the integer width code:

```text
0 omitted/reserved
1 i8
2 i16
3 i32
4 i64
5 i128
```

Timestamp slots are logical `i64` time values, but fixed-interval data can stamp
them as `fixed_step` with base and step constants in the aux table. The front
header schema map remains the source of parent relationships and repeated group
shape:

```text
0        no parent / root
1-99     direct parent slot ref; parent index = byte - 1
100      timestamp axis / timestamp parent ref
101-199  derived parent/root marker; slot number = byte - 100
200      dual-domain wrapper for the next group/node
201-239  group array: width = byte - 200
240      reserved
241      1-bit boolean stream
242      2-bit enum stream, up to 4 outcomes
243      bitfield stream, up to 8 flags
244-253  reserved
255      opaque / do-not-attempt stream
```

The front map describes relationship shape only. Constants, decimal scales,
residual streams, bit widths, and physical coding choices are stamped here in
the footer or stored in the body streams.

Scale is fixed-width and placed before any variable data so a parser can read it
with one predictable offset:

```text
scale_offset = slot_table_start + slot_index * slot_entry_size
```

## Decode tables

The body stores encoded values. The footer stores only the instructions and
constants needed to reconstruct logical values.

Slot `decode_kind` values are intentionally small and deterministic:

```text
fixed_width       Aura1 body stores the slot at the stamped width
fixed_step        body stores no slot values; aux stores base and step
stream            slot is decoded directly from a body stream
derived_offset    slot = parent slot + aux offset
derived_subtract  slot = parent slot - stream
group_member      slot is produced by a group transform
```

Streams are sequential in the body, so the stream table stores lengths, not
offsets:

```text
stream entry:
  repr        u8
  block_size  u8   0 for non-block streams, 255 for extended
  body_len    u32
  aux_id      u8   255 means no aux
```

Generic stream instructions:

```text
fixed_step       base + row_index * step
base_bitpack     base + packed_unsigned * storage_unit
prev_delta       previous + packed_delta * storage_unit
prev_varint      previous + zigzag-varint delta * storage_unit
block_local      body is divided into blocks, each with its own stream mode
patched_bitpack  low-width body plus exception indexes/high bits
rle              run lengths over a value stream
bitplane_rle     each bit plane is stored as runs
dictionary       dictionary entries plus bitpacked codes
packed_dictionary bitpacked dictionary entries plus bitpacked codes
uuid_const_mask  fixed 128-bit mask/value plus packed variable bits
```

The generic body codec is deterministic from the stamped stream instruction:

```text
fixed_step
  body: empty

base_bitpack
  body: packed_unsigned[value_count] using bit_width

prev_delta
  body: packed_signed[value_count - 1] using bit_width
  row 0 is the stamped base

prev_varint
  body: zigzag-varint scaled_delta[value_count - 1]
  row 0 is the stamped base

patched_bitpack
  body: packed low bits[value_count]
        packed exception indexes[exception_count]
        packed exception high bits[exception_count]

rle
  body: packed run values[run_count]
        varint run lengths[run_count]

bitplane_rle
  body per bit plane: start_bit u8, run_count u32, varint run lengths

dictionary
  body: signed-varint dictionary entries[entry_count]
        packed dictionary codes[value_count]

packed_dictionary
  body: packed dictionary entries[entry_count] using entry_width
        packed dictionary codes[value_count] using code_width

uuid_const_mask
  body: constant_mask u128, constant_value u128, packed variable bits

block_local
  body: one local mode header plus local stream body per fixed-size block
```

`block_local` modes are generic body-local choices (`fixed_step`,
`base_bitpack`, `prev_delta`, `patched_bitpack`, or `rle`) derived by the
writer from that block only. The footer instruction provides `block_size` and
the expected block count; the local body header provides only the constants
needed for that block.

Generic group instructions describe multi-slot structure without naming a
market-data domain:

```text
group                  event slots written once, repeated slots written per child
partition_runs         legacy fixed-order grouping metadata
partition_run_lengths  partition run values/order, run lengths, optional runs-per-event
group_value_stream     one event-level value expanded across each repeated group
segmented_delta_stream child values as first-per-run plus local deltas
presence_map           packed presence/enum bits for a set of slots
derived_stream         output slot reconstructed from input slots plus one stream
sparse_stream          nonzero values for one slot, selected by a presence_map bit
presence_value         constant nonzero value selected by a presence_map bit
```

A `derived_stream` is intentionally generic. Its operation can express common
curvefit shapes without hardcoding the source schema:

```text
add_residual              output = input + residual_stream
subtract_residual         output = input - residual_stream
max_plus_residual         output = max(input_a, input_b) + residual_stream
min_minus_residual        output = min(input_a, input_b) - residual_stream
first_offset_then_delta   first value from partition base, then local deltas
```

`partition_run_lengths`, `group_value_stream`, and `segmented_delta_stream`
cover repeated child layouts without domain names. The partition instruction
can store one fixed partition order or one partition value per run, plus one
run length per partition run. If events do not have a fixed number of partition
runs, it can also store one `runs_per_event` stream so event boundaries remain
unambiguous. A group value stream then stores event-level slots once per event,
and a segmented delta stream stores a repeated child slot as optional
partition-local bases, first value per run, and deltas inside each run.

`presence_map`, `sparse_stream`, and `presence_value` let the footer describe
zero-heavy slots without domain names. The writer may pack one presence mask for
multiple slots, store only nonzero values for sparse numeric slots, and derive
boolean-like slots directly from a presence bit. These are selected only when
the measured stamped body is smaller than direct slot streams. Sparse groups are
chosen by total bytes saved, not by smallest absolute sparse body, so a larger
multi-slot presence map can win when it removes more direct streams.

## Grouping

Timestamp grouping is separate from physical chunking. For repeated child data,
many logical rows can share one event. The footer should stamp the chosen
grouping strategy because it is part of decoding the body.

Physical chunks are optional and only needed for seeking, compression blocks,
parallel decode, or corruption isolation. `chunk_count` and a chunk table should
not be required for simple sequential bodies.

## Current implementation

The current code still uses the legacy twelve-byte trailer and has two footer
shapes:

```text
.aura        AURF ingest footer with stats and candidate plans
.aura0/.aura1 AURP compiled footer with a decode program
```

The compiled decode program already supports fixed steps, signed bitpacked
deltas, unsigned offset bitpacks, min/max residuals, product residuals, and
proportional residuals. `src/instructions.rs` defines a generic byte-aligned
stream/group instruction plan that can stamp curvefit shapes without
domain-specific operation names.

`src/generic_planner.rs` now derives executable generic stream/group plans from
the schema header hints and observed rows. It can plan and round-trip fixed
steps, base/previous bitpacking, previous zigzag-varints, patched bitpacking,
RLE, bitplane RLE, dictionary streams, packed dictionary streams, block-local
streams, UUID constant masks, sparse presence streams, candle-shape derived
streams, partition run lengths, grouped event-value streams, segmented child
deltas, and repeated-slot grouping. Parent relationships are scored as
transform candidates, not forced deltas; if a candidate body is larger than a
direct stream, the writer keeps the direct stream. The planner uses
relationships and scope bytes, not field names.

The `.aura` ingest footer now stamps a generic Aura0 plan alongside the legacy
field program candidates. `.aura -> .aura0` conversion follows that stamped
generic plan to write the body and stores the same plan in the compiled AURP
footer. Aura0 decode uses the generic plan when present, so the conversion path
does not re-optimize after ingest.

That should converge toward one small stamped slot table footer shared across
profiles, interpreted by the header `version` and `profile`.
