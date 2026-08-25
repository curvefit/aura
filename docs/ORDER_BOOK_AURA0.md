# Production explicit-event Aura0 repeated groups

Aura supports schema-declared repeated event data through the ordinary Aura0
header, compiled footer, stream framing, planner, writer, and decoder. The path
is provider-independent and does not select transforms from dataset names.

## Logical contract

This compact declaration describes two event values followed by a five-field
repeated group:

```text
[100, 0, 200, 205, 0, 0, 5, 0]
```

One useful logical interpretation is:

```text
event: [timestamp, sequence]
item:  [domain, key, QTY1, QTY2, count]
```

The bytes mean:

| Header byte | Logical effect |
|---:|---|
| `100` | Primary event timestamp, slot 0. |
| `0` | Independent event sequence, slot 1. |
| `200` | Marks the following repeated group as dual-domain. |
| `205` | Starts a five-field repeated group; its first field is the domain/discriminator, slot 2. |
| `0` | Independent repeated key, slot 3. |
| `0` | Independent repeated QTY1, slot 4. |
| `5` | Repeated QTY2 related to QTY1, output slot 5. |
| `0` | Independent repeated count, slot 6. |

Aura treats supplied integers as logical facts. Units, scaling, and nullability
belong to the schema or adapter and must be fixed before writing.

## Parent-child planning

The planner compares direct QTY2 encoding with both checked residual
orientations against QTY1. It includes instruction bytes in the cost and keeps
direct encoding on an exact score tie. Arithmetic is checked; overflow makes a
candidate inapplicable instead of wrapping.

The compiled footer records the selected plan. Decoding reconstructs QTY2 from
the recorded parent-child instruction without inspecting a filename, provider,
or dataset identity.

## Production API

```rust
use aura_codec::{
    generic_i64_parent_schema, AuraI64EventWriter, I64Event, Profile,
};

fn main() -> Result<(), aura_codec::AuraError> {
    let schema = generic_i64_parent_schema(
        "synthetic-qty2-v1",
        &[100, 0, 200, 205, 0, 0, 5, 0],
    )?;
    let mut writer = AuraI64EventWriter::new(schema);
    writer.push_event(I64Event {
        event_values: vec![1_000, 1],
        children: vec![
            vec![0, 100, 20, 19, 2],
            vec![1, 101, 30, 30, 3],
        ],
    })?;
    let aura0 = writer.finish_profile(Profile::Aura0)?;
    assert!(!aura0.is_empty());
    Ok(())
}
```

The same self-contained example is available as
`examples/order_book_aura0.rs`.

## Exactness

- Event boundaries come from explicit child counts, not timestamp or sequence
  run inference.
- Empty events and adjacent events with identical metadata remain distinct.
- Repeated source order and domain values are exact.
- QTY1, QTY2, and count values are exact integers.
- The decoder validates stream counts, group producers, relation authorization,
  residual arithmetic, body framing, footer length, and seal.

Select the relationship only from declared schema semantics. A name, venue,
ticker, path, or fixture identifier must never participate in planning.
