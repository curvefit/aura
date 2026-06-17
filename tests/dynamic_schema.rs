use aura_codec::schema::{
    decode_schema_map, generic_i64_parent_schema, generic_i64_schema, schema_parent_mapping,
    FieldRelation, FieldRole, FieldScope, FieldTransform, FieldType, RelatedFieldMapping,
    SchemaMapHint,
};
use aura_codec::{records, AuraError, FieldEncoding, Profile};

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn ingest_schema_block(file: &[u8]) -> (usize, &[u8]) {
    let seal_offset = file.len() - 8;
    let footer_len_offset = seal_offset - 4;
    let footer_len = read_u32_le(&file[footer_len_offset..seal_offset]) as usize;
    let footer_start = footer_len_offset - footer_len;
    let schema_len_offset = footer_start + 8;
    let schema_len = read_u32_le(&file[schema_len_offset..schema_len_offset + 4]) as usize;
    let schema_offset = schema_len_offset + 4;
    if schema_offset + schema_len > file.len() {
        return (schema_len, &[]);
    }
    (schema_len, &file[schema_offset..schema_offset + schema_len])
}

#[test]
fn generic_i64_schema_maps_first_field_to_time_and_related_values() {
    let schema = generic_i64_schema(
        "generic_ohlcv_like_v1",
        5,
        &[
            RelatedFieldMapping::new(2, 1),
            RelatedFieldMapping::new(3, 1),
            RelatedFieldMapping::new(4, 1),
        ],
    )
    .unwrap();

    assert_eq!(6, schema.fields.len());
    assert_eq!("ts", schema.fields[0].name);
    assert_eq!("v1", schema.fields[1].name);
    assert_eq!(FieldRelation::DeltaFromField(1), schema.fields[2].relation);
    assert!(schema.fields[0]
        .candidates
        .contains(FieldTransform::FixedStep));
    assert!(schema.fields[2]
        .candidates
        .contains(FieldTransform::DeltaRelated));
    assert!(schema.fields[5]
        .candidates
        .contains(FieldTransform::DeltaPrevious));
    assert!(!schema.fields[5]
        .candidates
        .contains(FieldTransform::DeltaRelated));
}

#[test]
fn generic_i64_schema_drives_compiled_field_choices_without_ohlcv_names() {
    let schema = generic_i64_schema(
        "generic_ohlcv_like_v1",
        5,
        &[
            RelatedFieldMapping::new(2, 1),
            RelatedFieldMapping::new(3, 1),
            RelatedFieldMapping::new(4, 1),
        ],
    )
    .unwrap();
    let rows = vec![
        vec![1_000_000_000, 10_000, 10_010, 9_990, 10_005, 1_000_000],
        vec![61_000_000_000, 20_000, 20_010, 19_990, 20_005, 1_000_007],
        vec![121_000_000_000, 10_000, 10_010, 9_990, 10_005, 1_000_003],
    ];
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema: schema.clone(),
        rows: rows.clone(),
        stream_id: 42,
        dictionary_id: 7,
        header_comment: None,
    })
    .unwrap();

    let decoded_ingest = records::decode_i64_file(&ingest).unwrap();
    let plan = decoded_ingest
        .ingest_footer
        .as_ref()
        .unwrap()
        .aura0_plan
        .as_ref()
        .unwrap();

    assert_eq!(
        FieldEncoding::ImplicitFixedStep,
        plan.field("ts", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::DerivedOffset,
        plan.field("v2", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::BitpackedDeltaPrevious,
        plan.field("v5", &schema).unwrap().encoding
    );

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let decoded_aura0 = records::decode_i64_file(&aura0).unwrap();

    assert_eq!(rows, decoded_aura0.rows);
    assert_eq!(schema.fields, decoded_aura0.schema.fields);
}

#[test]
fn generic_i64_schema_rejects_invalid_related_mappings() {
    let result = generic_i64_schema("bad_mapping_v1", 5, &[RelatedFieldMapping::new(9, 1)]);

    assert_eq!(Err(AuraError::InvalidValue("related field index")), result);
}

#[test]
fn generic_i64_parent_schema_maps_time_and_parent_bytes() {
    let schema =
        generic_i64_parent_schema("dynamic_ohlcv_plus_flow_v1", &[100, 0, 2, 2, 2, 0, 6, 6])
            .unwrap();

    assert_eq!(8, schema.fields.len());
    assert_eq!("ts", schema.fields[0].name);
    assert_eq!("v1", schema.fields[1].name);
    assert_eq!("v7", schema.fields[7].name);
    assert_eq!(FieldRelation::DeltaFromField(1), schema.fields[2].relation);
    assert_eq!(FieldRelation::DeltaFromField(1), schema.fields[3].relation);
    assert_eq!(FieldRelation::DeltaFromField(1), schema.fields[4].relation);
    assert_eq!(FieldRelation::None, schema.fields[5].relation);
    assert_eq!(FieldRelation::DeltaFromField(5), schema.fields[6].relation);
    assert_eq!(FieldRelation::DeltaFromField(5), schema.fields[7].relation);
}

#[test]
fn generic_i64_parent_schema_maps_repeated_child_scope() {
    let schema =
        generic_i64_parent_schema("dynamic_book_delta_v1", &[100, 0, 0, 205, 0, 0, 0, 0]).unwrap();

    assert_eq!(FieldScope::Event, schema.fields[0].scope);
    assert_eq!(FieldScope::Event, schema.fields[1].scope);
    assert_eq!(FieldScope::Event, schema.fields[2].scope);
    assert_eq!(FieldScope::Repeated, schema.fields[3].scope);
    assert_eq!(FieldScope::Repeated, schema.fields[7].scope);
    assert_eq!(FieldRelation::None, schema.fields[3].relation);
}

#[test]
fn generic_i64_parent_schema_maps_extended_header_bytes() {
    let parent_slots = [100, 101, 2, 204, 4, 241, 242, 243, 255];
    let entries = decode_schema_map(&parent_slots).unwrap();

    assert_eq!(SchemaMapHint::Timestamp, entries[0].hint);
    assert_eq!(
        SchemaMapHint::DerivedRoot { slot_number: 1 },
        entries[1].hint
    );
    assert_eq!(FieldRelation::DeltaFromField(1), entries[2].relation);
    assert_eq!(SchemaMapHint::Group { width: 4 }, entries[3].hint);
    assert_eq!(FieldScope::Repeated, entries[3].scope);
    assert_eq!(FieldScope::Repeated, entries[4].scope);
    assert_eq!(FieldScope::Repeated, entries[5].scope);
    assert_eq!(FieldScope::Repeated, entries[6].scope);
    assert_eq!(FieldScope::Event, entries[7].scope);
    assert_eq!(SchemaMapHint::Boolean { bits: 1 }, entries[5].hint);
    assert_eq!(SchemaMapHint::Enum { bits: 2 }, entries[6].hint);
    assert_eq!(SchemaMapHint::Bitfield { bits: 8 }, entries[7].hint);
    assert_eq!(SchemaMapHint::DoNotAttempt, entries[8].hint);

    let schema = generic_i64_parent_schema("extended_header_v1", &parent_slots).unwrap();
    assert_eq!(FieldRole::Boolean, schema.fields[5].role);
    assert_eq!(FieldRole::Enum, schema.fields[6].role);
    assert_eq!(FieldRole::Bitfield, schema.fields[7].role);
    assert_eq!(FieldType::Opaque16, schema.fields[8].field_type);
    assert_eq!(
        parent_slots.as_slice(),
        schema_parent_mapping(&schema).unwrap().as_slice()
    );
}

#[test]
fn generic_i64_parent_schema_accepts_non_time_series_maps() {
    let parent_slots = [0, 0, 241];
    let schema = generic_i64_parent_schema("non_time_series_v1", &parent_slots).unwrap();

    assert_eq!("v0", schema.fields[0].name);
    assert_eq!(FieldRole::Value, schema.fields[0].role);
    assert_eq!(FieldRole::Boolean, schema.fields[2].role);

    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: vec![vec![10, 20, 1], vec![11, 21, 0]],
        stream_id: 0,
        dictionary_id: 0,
        header_comment: None,
    })
    .unwrap();
    let decoded = records::decode_i64_file(&ingest).unwrap();

    assert_eq!(0, decoded.header.base_time_ns);
    assert_eq!(
        parent_slots.as_slice(),
        decoded.header.schema_mapping.as_slice()
    );
}

#[test]
fn generic_i64_writer_preserves_scoped_parent_map_in_header() {
    let parent_slots = vec![100, 0, 0, 205, 0, 0, 0, 0];
    let schema = generic_i64_parent_schema("dynamic_book_delta_v1", &parent_slots).unwrap();
    let rows = vec![
        vec![1_000, 10, 20, 1, 0, 100_000, 5, 0],
        vec![1_000, 10, 20, 1, 0, 100_010, 0, 1],
        vec![1_001, 11, 21, 1, 1, 100_020, 8, 0],
    ];

    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 42,
        dictionary_id: 7,
        header_comment: None,
    })
    .unwrap();
    let decoded = records::decode_i64_file(&ingest).unwrap();

    assert_eq!(parent_slots, decoded.header.schema_mapping);
    assert_eq!(FieldScope::Repeated, decoded.schema.fields[3].scope);
}

#[test]
fn generic_i64_parent_schema_drives_dynamic_aura0_related_deltas() {
    let schema =
        generic_i64_parent_schema("dynamic_ohlcv_plus_flow_v1", &[100, 0, 2, 2, 2, 0, 6, 6])
            .unwrap();
    let rows = vec![
        vec![
            1_000_000_000,
            10_000,
            10_010,
            9_990,
            10_005,
            1_000,
            1_001,
            999,
        ],
        vec![
            61_000_000_000,
            20_000,
            20_010,
            19_990,
            20_005,
            100_000,
            100_001,
            99_999,
        ],
        vec![
            121_000_000_000,
            10_000,
            10_010,
            9_990,
            10_005,
            10_000_000,
            10_000_001,
            9_999_999,
        ],
    ];
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema: schema.clone(),
        rows: rows.clone(),
        stream_id: 42,
        dictionary_id: 7,
        header_comment: None,
    })
    .unwrap();

    let decoded_ingest = records::decode_i64_file(&ingest).unwrap();
    let plan = decoded_ingest
        .ingest_footer
        .as_ref()
        .unwrap()
        .aura0_plan
        .as_ref()
        .unwrap();

    assert_eq!(
        FieldEncoding::DerivedOffset,
        plan.field("v2", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::DerivedOffset,
        plan.field("v6", &schema).unwrap().encoding
    );
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    assert_eq!(rows, records::decode_i64_file(&aura0).unwrap().rows);
}

#[test]
fn generic_i64_parent_schema_uses_length_prefixed_parent_encoding() {
    let schema =
        generic_i64_parent_schema("dynamic_ohlcv_plus_flow_v1", &[100, 0, 2, 2, 2, 0, 6, 6])
            .unwrap();
    let rows = vec![vec![
        1_000_000_000,
        10_000,
        10_010,
        9_990,
        10_005,
        1_000,
        1_001,
        999,
    ]];

    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 42,
        dictionary_id: 7,
        header_comment: None,
    })
    .unwrap();

    let (schema_len, schema_encoding) = ingest_schema_block(&ingest);

    assert_eq!(10, schema_len);
    assert_eq!(&[0, 8, 100, 0, 2, 2, 2, 0, 6, 6], schema_encoding);
}

#[test]
fn generic_i64_parent_schema_rejects_forward_and_self_parents() {
    assert_eq!(
        Err(AuraError::InvalidValue("parent slot")),
        generic_i64_parent_schema("bad_self_parent", &[100, 2])
    );
    assert_eq!(
        Err(AuraError::InvalidValue("parent slot")),
        generic_i64_parent_schema("bad_forward_parent", &[100, 0, 4])
    );
}
