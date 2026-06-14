# Footer stamp

The Aura footer should be a stamped decode manifest, not planning evidence.
During ingest the writer may collect stats, test candidates, and choose a
layout. At stamp time it writes the final footer once. After that the footer is
constant unless the file is restamped.

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
number of logical slots and must equal the front header `schema_len`. Slot order
is the logical field identity; field names live in the header comment or the
external stream dictionary, not in the footer.

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
header schema map remains the source of parent relationships and repeated child
shape:

```text
255 primary timestamp
0 event/root slot
1..127 event slot, parent = value - 1
128 repeated child root slot
129..254 repeated child slot, parent = value - 129
```

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
block_local      body is divided into blocks, each with its own stream mode
patched_bitpack  low-width body plus exception indexes/high bits
rle              run lengths over a value stream
bitplane_rle     each bit plane is stored as runs
dictionary       dictionary entries plus bitpacked codes
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

uuid_const_mask
  body: constant_mask u128, constant_value u128, packed variable bits

block_local
  body: one local mode header plus local stream body per fixed-size block
```

`block_local` modes are generic body-local choices (`fixed_step`,
`base_bitpack`, `patched_bitpack`, or `rle`) derived by the writer from that
block only. The footer instruction provides `block_size` and the expected block
count; the local body header provides only the constants needed for that block.

Generic group instructions describe multi-slot structure without naming a
market-data domain:

```text
group            event slots written once, repeated slots written per child
partition_runs   contiguous runs of a partition slot, with optional fixed order
presence_map     packed presence/enum bits for a set of slots
derived_stream   output slot reconstructed from input slots plus one stream
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

## Grouping

Timestamp grouping is separate from physical chunking. For repeated child data,
many logical rows can share one event. The footer should stamp the chosen
grouping strategy because it is part of decoding the body.

Physical chunks are optional and only needed for seeking, compression blocks,
parallel decode, or corruption isolation. `chunk_count` and a chunk table should
not be required for simple sequential bodies.

## Current implementation gap

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
domain-specific operation names. The main `.aura0` writer still uses the
field-program footer; it needs to converge on the generic instruction plan for
the fitted layouts to be emitted by the production writer.

That should converge toward one small stamped slot table footer shared across
profiles, interpreted by the header `version` and `profile`.
