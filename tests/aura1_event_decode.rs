use aura_codec::program::CompiledFooter;
use aura_codec::records::{
    aura1_fixed_layout_info, decode_i64_file_metadata, Aura1FixedLayoutInfo,
};
use aura_codec::{
    AuraError, AuraI64EventReader, AuraI64EventWriter, CompiledAuraPlan, FieldRole, FieldType,
    I64Event, PhysicalWidth, Profile, SchemaBuilder, SchemaDescriptor,
};

const EXPLICIT_EVENT_SIDECAR_TRAILER: usize = 12;

fn boundaries_schema() -> SchemaDescriptor {
    SchemaBuilder::new("aura1-event-boundaries")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("reset", FieldType::U8, FieldRole::Boolean)
        .field("sequence", FieldType::I64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .finish()
        .unwrap()
}

fn aura1_bytes(schema: SchemaDescriptor, events: &[I64Event]) -> Vec<u8> {
    let mut writer = AuraI64EventWriter::new(schema);
    for event in events.iter().cloned() {
        writer.push_event(event).unwrap();
    }
    writer.finish_profile(Profile::Aura1).unwrap()
}

fn corruption_events() -> Vec<I64Event> {
    vec![
        I64Event {
            event_values: vec![100, 10],
            children: vec![vec![1, 1_000], vec![2, 2_000]],
        },
        I64Event {
            event_values: vec![101, 11],
            children: vec![vec![3, 3_000]],
        },
    ]
}

fn corruption_fixture() -> (Vec<u8>, Aura1FixedLayoutInfo, CompiledFooter) {
    let bytes = aura1_bytes(
        SchemaBuilder::new("aura1-event-corruption")
            .field("ts", FieldType::I64, FieldRole::Timestamp)
            .field("sequence", FieldType::I64, FieldRole::Sequence)
            .repeated_field("side", FieldType::U8, FieldRole::Side)
            .repeated_field("value", FieldType::I64, FieldRole::Value)
            .finish()
            .unwrap(),
        &corruption_events(),
    );
    let info = aura1_fixed_layout_info(&bytes).unwrap();
    let footer = decode_i64_file_metadata(&bytes)
        .unwrap()
        .compiled_footer
        .unwrap();
    (bytes, info, footer)
}

fn skip_varint(bytes: &[u8], mut offset: usize) -> usize {
    while bytes[offset] & 0x80 != 0 {
        offset += 1;
    }
    offset + 1
}

fn sidecar_start(info: Aura1FixedLayoutInfo) -> usize {
    info.body_offset + info.body_bytes
}

fn sidecar_length_offset(info: Aura1FixedLayoutInfo) -> usize {
    info.footer_offset - EXPLICIT_EVENT_SIDECAR_TRAILER
}

fn overwrite_aura1_field(
    bytes: &mut [u8],
    info: Aura1FixedLayoutInfo,
    footer: &CompiledFooter,
    row_index: usize,
    field_index: u16,
    value: i64,
) {
    let compiled = CompiledAuraPlan::from_footer(footer).unwrap();
    let field = compiled.aura1_field_offset(field_index).unwrap();
    let width = compiled
        .aura1_plan
        .fields
        .iter()
        .find(|plan| plan.field_index == field_index)
        .unwrap()
        .width;
    let start = info.body_offset + row_index * info.record_width + field.offset;
    let target = &mut bytes[start..start + field.width];
    match width {
        PhysicalWidth::Zero => assert_eq!(value, 0),
        PhysicalWidth::I8 => target[0] = i8::try_from(value).unwrap() as u8,
        PhysicalWidth::I16 => target.copy_from_slice(&i16::try_from(value).unwrap().to_le_bytes()),
        PhysicalWidth::I32 => target.copy_from_slice(&i32::try_from(value).unwrap().to_le_bytes()),
        PhysicalWidth::I64 => target.copy_from_slice(&value.to_le_bytes()),
        PhysicalWidth::I128 => target.copy_from_slice(&i128::from(value).to_le_bytes()),
    }
}

#[test]
fn aura1_event_decode_preserves_empty_reset_and_adjacent_equal_boundaries() {
    let schema = boundaries_schema();
    let empty = aura1_bytes(schema.clone(), &[]);
    assert_eq!(AuraI64EventReader::open(&empty).unwrap().events(), &[]);

    let expected = vec![
        I64Event {
            event_values: vec![1_000, 0, 7],
            children: vec![vec![0, 100, 8], vec![1, 101, 9]],
        },
        I64Event {
            event_values: vec![1_001, 1, 8],
            children: vec![],
        },
        I64Event {
            event_values: vec![1_002, 0, 9],
            children: vec![vec![0, 102, 10]],
        },
        // Equal headers are still two source events, with independent child
        // boundaries and values.
        I64Event {
            event_values: vec![1_002, 0, 9],
            children: vec![vec![1, 103, 11], vec![0, 104, 12]],
        },
    ];
    let bytes = aura1_bytes(schema, &expected);
    assert_eq!(AuraI64EventReader::open(&bytes).unwrap().events(), expected);
}

#[test]
fn aura1_event_decode_preserves_interleaved_scopes_mixed_widths_and_extremes() {
    let schema = SchemaBuilder::new("aura1-event-interleaved-scopes")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .field("reset", FieldType::I8, FieldRole::Flag)
        .repeated_field("price", FieldType::I16, FieldRole::Price)
        .field("sequence", FieldType::I32, FieldRole::Sequence)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .field("marker", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let expected = vec![
        I64Event {
            event_values: vec![i64::MIN + 7, -7, -2_000_000_000, i64::MAX],
            children: vec![
                vec![1, i16::MIN as i64, i64::MIN],
                vec![2, i16::MAX as i64, i64::MAX],
            ],
        },
        I64Event {
            event_values: vec![i64::MAX - 7, 7, 2_000_000_000, i64::MIN],
            children: vec![vec![255, -1, 0]],
        },
    ];
    let bytes = aura1_bytes(schema, &expected);
    assert_eq!(AuraI64EventReader::open(&bytes).unwrap().events(), expected);
}

#[test]
fn aura1_event_decode_rejects_corrupted_per_row_event_header() {
    let (mut bytes, info, footer) = corruption_fixture();
    // The second child still belongs to the first event. Change only its
    // absolute event-scope timestamp so the child count remains valid.
    overwrite_aura1_field(&mut bytes, info, &footer, 1, 0, 101);
    assert_eq!(
        AuraI64EventReader::open(&bytes).unwrap_err(),
        AuraError::InvalidValue("event value mismatch")
    );
}

#[test]
fn aura1_event_decode_rejects_corrupted_sidecar_counts_and_field_count() {
    let (bytes, info, _) = corruption_fixture();
    let start = sidecar_start(info);

    let mut child_count = bytes.clone();
    let counts_start = skip_varint(&child_count, skip_varint(&child_count, start));
    assert_eq!(child_count[counts_start], 2);
    child_count[counts_start] = 1;
    assert_eq!(
        AuraI64EventReader::open(&child_count).unwrap_err(),
        AuraError::InvalidValue("child count")
    );

    let mut event_count = bytes.clone();
    event_count[start] = 0xff;
    assert_eq!(
        AuraI64EventReader::open(&event_count).unwrap_err(),
        AuraError::InvalidValue("event count")
    );

    let mut event_field_count = bytes;
    let field_count_offset = skip_varint(&event_field_count, start);
    assert_eq!(event_field_count[field_count_offset], 2);
    event_field_count[field_count_offset] = 1;
    assert_eq!(
        AuraI64EventReader::open(&event_field_count).unwrap_err(),
        AuraError::InvalidValue("event field count")
    );
}

#[test]
fn aura1_event_decode_rejects_trailing_sidecar_bytes() {
    let (mut bytes, info, _) = corruption_fixture();
    let start = sidecar_start(info);
    let length_offset = sidecar_length_offset(info);
    let sidecar_len =
        u64::from_le_bytes(bytes[length_offset..length_offset + 8].try_into().unwrap()) as usize;
    assert_eq!(start + sidecar_len, length_offset);
    bytes.insert(length_offset, 0xaa);
    let new_length_offset = length_offset + 1;
    bytes[new_length_offset..new_length_offset + 8]
        .copy_from_slice(&((sidecar_len + 1) as u64).to_le_bytes());
    assert_eq!(
        AuraI64EventReader::open(&bytes).unwrap_err(),
        AuraError::TrailingBytes(1)
    );
}

#[test]
fn aura1_event_decode_rejects_truncated_fixed_body() {
    let (mut bytes, info, _) = corruption_fixture();
    assert!(info.body_bytes > 0);
    bytes.remove(info.body_offset + info.body_bytes - 1);
    assert_eq!(
        AuraI64EventReader::open(&bytes).unwrap_err(),
        AuraError::InvalidValue("aura1 body length")
    );
}

#[test]
fn aura1_event_decode_rejects_unsupported_footer_version() {
    let (mut bytes, info, _) = corruption_fixture();
    // A compiled footer starts with AURP followed by its u16 container
    // version. Keep the seal and footer length intact so only version
    // validation is exercised.
    bytes[info.footer_offset + 4..info.footer_offset + 6].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(
        AuraI64EventReader::open(&bytes).unwrap_err(),
        AuraError::UnsupportedVersion(1)
    );
}
