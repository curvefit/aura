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

Timestamp slots are logical `i64` time values, but candle data can stamp them as
`fixed_step` with base and step constants in the aux table. The front header
schema map remains the source of parent relationships:

```text
255 primary timestamp | 0 no parent | 1..254 parent slot + 1
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

Common stream representations:

```text
fixed_i8/i16/i32/i64/i128
fixed_u8/u16/u32/u64/u128
zigzag_varint
unsigned_varint
zigzag_bitpack
unsigned_bitpack
block_min_bitpack
sparse_zigzag_varint
sparse_unsigned_varint
```

Group entries describe multi-slot transforms. The compact candle-shape group is
four bytes and requires consecutive OHLC slots and consecutive streams:

```text
group_op      u8  CANDLE_SHAPE
first_slot    u8  open slot; high, low, close follow
first_stream  u8  open_delta stream; body, upper, lower follow
aux_id        u8  base_open
```

Decode:

```text
open  = previous_close + open_delta
close = open + body
high  = max(open, close) + upper_wick
low   = min(open, close) - lower_wick
```

For Binance BTCUSDT klines, a stamped Aura0 footer can express:

```text
open_time       fixed_step(base_ts, 60000)
open..close     group_member(CANDLE_SHAPE)
volume          stream(block_min_bitpack)
close_time      derived_offset(open_time, 59999)
quote_volume    stream(block_min_bitpack)
trades          stream(block_min_bitpack)
taker_buy_base  derived_subtract(volume, sell_base_stream)
taker_buy_quote derived_subtract(quote_volume, sell_quote_stream)
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

The current code still uses the legacy twelve-byte trailer and has two footer
shapes:

```text
.aura        AURF ingest footer with stats and candidate plans
.aura0/.aura1 AURP compiled footer with a decode program
```

The compiled decode program already supports fixed steps, signed bitpacked
deltas, unsigned offset bitpacks, candle wick residuals, product residuals, and
proportional residuals. It is still a field-program footer rather than the target
slot-tail footer above.

That should converge toward one small stamped slot table footer shared across
profiles, interpreted by the header `version` and `profile`.
