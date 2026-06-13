# Footer stamp

The Aura footer should be a stamped decode manifest, not planning evidence.
During ingest the writer may collect stats, test candidates, and choose a
layout. At stamp time it writes the final footer once. After that the footer is
constant unless the file is restamped.

The trailer locates the footer:

```text
footer_len  u32 little-endian
sealed:)
```

The trailing `sealed:)` bytes are the file magic for the stamped end state, so
the footer does not need a second magic string. The header already carries the
file `version` and `profile`, so the footer should not repeat the encoding type.

## Core fields

The footer core should stay small:

```text
record_count  u64
slot_count    u8
slot_table    slot_count fixed entries
aux_table     constants required by slot instructions
```

`record_count` is the number of logical events in the body. `slot_count` is the
number of logical slots. The slot table mirrors the header schema map, but each
slot also carries the stamped parsing facts needed on the hot path.

Each fixed slot entry starts as:

```text
schema_byte  u8  255 primary timestamp | 0 no parent | 1..254 parent slot + 1
scale        i8  decimal scale used to normalize this slot
instruction  u8  stamped physical operation, width, and flags
```

For hot `.aura1` slots, the first hot instruction byte is the fixed integer
width for non-timestamp slots:

```text
0 omitted/reserved
1 i8
2 i16
3 i32
4 i64
5 i128
```

Timestamp slots are always logical `i64` nanoseconds; their hot byte is the
stamped timestamp grouping code instead of an integer-width code.

Scale is fixed-width and placed before any variable data so a parser can read it
with one predictable offset:

```text
scale_offset = slot_table_start + slot_index * 3 + 1
```

## Instructions

The body stores encoded values. The footer stores only the instructions and
constants needed to reconstruct logical values.

Examples:

```text
absolute        body stores the value at the stamped width
delta_base      body stores value - base; aux stores base
delta_previous  body stores value - previous; aux may store first/base
delta_related   body stores value - parent slot value
fixed_step      body stores no value; aux stores base and step
```

For `ts, open, high, low, close, volume`, a stamped footer might look like:

```text
slot  schema  scale  instruction        aux
0     255     0      fixed_step         base_ts, step_ns
1     0       2      delta_base_i16     base_open
2     2       2      delta_related_i16  -
3     2       2      delta_related_i16  -
4     2       2      delta_related_i16  -
5     0       0      delta_base_i16     base_volume
```

## Grouping

Timestamp grouping is separate from physical chunking. For websocket-style
orderbook updates, many logical events can share one timestamp. The footer
should stamp the chosen timestamp grouping strategy, such as fixed power-of-two
group capacity, because that is part of decoding the body.

Physical chunks are optional and only needed for seeking, compression blocks,
parallel decode, or corruption isolation. `chunk_count` and a chunk table should
not be required for simple sequential bodies.

## Current implementation gap

The current code still has two footer shapes:

```text
.aura        AURF ingest footer with stats and candidate plans
.aura0/.aura1 AURP compiled footer with a decode program
```

That should converge toward one small stamped slot table footer shared across
profiles, interpreted by the header `version` and `profile`.
