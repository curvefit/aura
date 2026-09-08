use aura_codec::generic_planner::{decode_generic_i64_events_body, encode_generic_i64_events};
use aura_codec::records::MAX_V2_I64_DECODE_VALUES;
use aura_codec::{
    encode_generic_i64_rows_body, AuraError, FieldRole, FieldType, I64Event, SchemaBuilder,
};

fn schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("generic-event-decode")
        .field("event_time", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("reset", FieldType::U8, FieldRole::Boolean)
        .repeated_field("side", FieldType::U8, FieldRole::Enum)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .finish()
        .unwrap()
}

fn events() -> Vec<I64Event> {
    vec![
        I64Event {
            event_values: vec![1_000, 0],
            children: vec![vec![0, 100]],
        },
        I64Event {
            event_values: vec![1_001, 1],
            children: vec![],
        },
        I64Event {
            event_values: vec![1_001, 0],
            children: vec![vec![1, 101]],
        },
    ]
}

#[test]
fn explicit_event_body_decode_preserves_boundaries_and_values() {
    let schema = schema();
    let events = events();
    let event_values = events
        .iter()
        .map(|event| event.event_values.clone())
        .collect::<Vec<_>>();
    let children = events
        .iter()
        .map(|event| event.children.clone())
        .collect::<Vec<_>>();
    let encoded = encode_generic_i64_events(&schema, &event_values, &children).unwrap();
    let body = encode_generic_i64_rows_body(&encoded).unwrap();

    let decoded =
        decode_generic_i64_events_body(&schema, encoded.plan, &body, encoded.record_count).unwrap();
    assert_eq!(decoded.event_values, event_values);
    assert_eq!(decoded.children, children);
}

#[test]
fn explicit_event_body_decode_keeps_stream_and_total_value_limits() {
    let schema = schema();
    let events = events();
    let event_values = events
        .iter()
        .map(|event| event.event_values.clone())
        .collect::<Vec<_>>();
    let children = events
        .iter()
        .map(|event| event.children.clone())
        .collect::<Vec<_>>();
    let encoded = encode_generic_i64_events(&schema, &event_values, &children).unwrap();
    let body = encode_generic_i64_rows_body(&encoded).unwrap();

    let mut too_many_streams = body.clone();
    let stream_count = u16::try_from(schema.fields.len() * 16 + 1).unwrap();
    too_many_streams[..2].copy_from_slice(&stream_count.to_le_bytes());
    assert_eq!(
        decode_generic_i64_events_body(
            &schema,
            encoded.plan.clone(),
            &too_many_streams,
            encoded.record_count,
        )
        .unwrap_err(),
        AuraError::InvalidValue("generic stream count")
    );

    // Framed headers are scanned and their declared values are bounded before
    // any codec allocates its output vector. The second frame starts after the
    // first 14-byte header and its body.
    let first_body_len = u32::from_le_bytes(body[12..16].try_into().unwrap()) as usize;
    let second_header = 2 + 14 + first_body_len;
    let declared_values = (MAX_V2_I64_DECODE_VALUES / 2 + 1) as u64;
    let mut too_many_values = body;
    too_many_values[4..12].copy_from_slice(&declared_values.to_le_bytes());
    too_many_values[second_header + 2..second_header + 10]
        .copy_from_slice(&declared_values.to_le_bytes());
    assert_eq!(
        decode_generic_i64_events_body(
            &schema,
            encoded.plan,
            &too_many_values,
            encoded.record_count,
        )
        .unwrap_err(),
        AuraError::InvalidValue("generic stream value limit")
    );
}
