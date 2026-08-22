use aura_codec::footer::AuraFooter;
use aura_codec::instructions::GenericStreamOp;
use aura_codec::schema::{
    generic_i64_parent_schema, FieldRelation, FieldRole, FieldTransform, FieldType, SchemaBuilder,
};
use aura_codec::{
    records, AuraError, AuraHeader, AuraTypedReader, AuraTypedValue, AuraTypedWriter,
    DerivedExpression, DerivedExpressionOp, DerivedExpressionSource, IngestStats, PhysicalWidth,
    Profile,
};

fn typed_wide_schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("typed_wide_v1")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("exec_id", FieldType::Opaque16, FieldRole::Identifier)
        .field("notional", FieldType::I128, FieldRole::Value)
        .finish()
        .unwrap()
}

fn typed_uuid_schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("typed_uuid_v1")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("exec_id", FieldType::Opaque16, FieldRole::Identifier)
        .field("price", FieldType::I64, FieldRole::Price)
        .finish()
        .unwrap()
}

fn diagnostic(error: AuraError) -> aura_codec::AuraDiagnostic {
    match error {
        AuraError::Diagnostic(diagnostic) => diagnostic,
        other => panic!("expected diagnostic error, got {other:?}"),
    }
}

#[test]
fn typed_writer_defaults_to_i64_and_accepts_declared_wide_values() {
    let schema = generic_i64_parent_schema("typed_default_i64", &[100, 0, 2]).unwrap();
    let mut writer = AuraTypedWriter::new(schema).with_stream(7, 3);
    writer
        .extend_rows([
            vec![1_000.into(), 10.into(), 12.into()],
            vec![2_000.into(), 11.into(), 13.into()],
        ])
        .unwrap();
    let file = writer.finish().unwrap();
    let decoded = records::decode_i64_file(&file).unwrap();

    assert_eq!(Profile::Ingest, decoded.header.profile);
    assert_eq!(vec![vec![1_000, 10, 12], vec![2_000, 11, 13]], decoded.rows);

    let wide_schema = typed_wide_schema();
    let mut wide_writer = AuraTypedWriter::new(wide_schema.clone());
    wide_writer
        .push_row(vec![
            AuraTypedValue::I64(1_000),
            AuraTypedValue::Opaque16([0xAB; 16]),
            AuraTypedValue::I128(i128::from(i64::MAX) + 1),
        ])
        .unwrap();

    let footer = AuraFooter::new(
        wide_schema.clone(),
        IngestStats::new_for_schema(&wide_schema).unwrap(),
    );
    let decoded_footer = AuraFooter::decode(&footer.encode().unwrap()).unwrap();

    assert_eq!(
        FieldType::Opaque16,
        decoded_footer.schema.fields[1].field_type
    );
    assert_eq!(FieldType::I128, decoded_footer.schema.fields[2].field_type);
}

#[test]
fn wide_values_roundtrip_through_typed_ingest() {
    let schema = typed_wide_schema();
    let mut writer = AuraTypedWriter::new(schema);
    let rows = vec![vec![
        AuraTypedValue::I64(1_000),
        AuraTypedValue::Opaque16([1; 16]),
        AuraTypedValue::I128(i128::from(i64::MAX) + 10),
    ]];
    writer.push_row(rows[0].clone()).unwrap();

    let file = writer.finish().unwrap();
    let decoded = records::decode_typed_file(&file).unwrap();
    let reader = AuraTypedReader::open(&file).unwrap();

    assert_eq!(rows, decoded.rows);
    assert_eq!(rows, reader.rows());
    assert!(records::decode_i64_file(&file).is_err());
    assert!(records::compile_i64_file(&file, Profile::Aura0).is_err());
    assert!(records::compile_typed_file(&file, Profile::Aura0).is_err());
}

#[test]
fn opaque16_roundtrips_through_one_logical_aura0_stream() {
    let schema = typed_uuid_schema();
    let rows = (0..1_024u64)
        .map(|index| {
            let mut id = [0u8; 16];
            id[..8].copy_from_slice(&index.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());
            id[8..].copy_from_slice(&index.rotate_left(19).to_le_bytes());
            id[6] = (id[6] & 0x0F) | 0x40;
            id[8] = (id[8] & 0x3F) | 0x80;
            vec![
                AuraTypedValue::I64(1_700_000_000_000_000_000 + index as i64),
                AuraTypedValue::Opaque16(id),
                AuraTypedValue::I64(20_000 + (index % 17) as i64),
            ]
        })
        .collect::<Vec<_>>();
    let ingest = records::encode_ingest_typed_file(records::TypedFileInput {
        schema: schema.clone(),
        rows: rows.clone(),
        stream_id: 7,
        dictionary_id: 3,
        header_comment: Some("opaque uuid".to_owned()),
    })
    .unwrap();
    let aura0 = records::compile_typed_file(&ingest, Profile::Aura0).unwrap();
    let decoded = records::decode_typed_file(&aura0).unwrap();

    assert_eq!(Profile::Aura0, decoded.header.profile);
    assert_eq!(rows, decoded.rows);
    assert_eq!(schema, decoded.schema);
    assert!(records::decode_i64_file(&aura0).is_err());
    assert!(aura0.len() < ingest.len());

    let plan = decoded.compiled_footer.unwrap().generic_aura0_plan.unwrap();
    let uuid_streams = plan
        .streams
        .iter()
        .filter(|stream| matches!(stream.op, GenericStreamOp::UuidConstMask { .. }))
        .collect::<Vec<_>>();
    assert_eq!(1, uuid_streams.len());
    assert_eq!(Some(1), uuid_streams[0].target_slot);
    assert_eq!(3, schema.fields.len());

    let aura1 = records::compile_typed_file(&aura0, Profile::Aura1).unwrap();
    assert_eq!(rows, records::decode_typed_file(&aura1).unwrap().rows);
    assert_eq!(rows, AuraTypedReader::open(&aura1).unwrap().rows());
    let aura0_again = records::compile_typed_file(&aura1, Profile::Aura0).unwrap();
    assert_eq!(rows, records::decode_typed_file(&aura0_again).unwrap().rows);
    assert_eq!(aura0, aura0_again);

    let direct_aura1 = records::compile_typed_file(&ingest, Profile::Aura1).unwrap();
    assert_eq!(
        rows,
        records::decode_typed_file(&direct_aura1).unwrap().rows
    );

    let uuid_stream_id = uuid_streams[0].stream_id;
    let mut malformed = aura0.clone();
    let mut offset = AuraHeader::encoded_len(&malformed).unwrap();
    let stream_count = u16::from_le_bytes([malformed[offset], malformed[offset + 1]]) as usize;
    offset += 2;
    let mut uuid_body_offset = None;
    for _ in 0..stream_count {
        let stream_id = u16::from_le_bytes([malformed[offset], malformed[offset + 1]]);
        let body_len = u32::from_le_bytes([
            malformed[offset + 10],
            malformed[offset + 11],
            malformed[offset + 12],
            malformed[offset + 13],
        ]) as usize;
        offset += 14;
        if stream_id == uuid_stream_id {
            uuid_body_offset = Some(offset);
        }
        offset += body_len;
    }
    malformed[uuid_body_offset.unwrap()] ^= 1;
    assert!(records::decode_typed_file(&malformed).is_err());
}

#[test]
fn typed_compile_delegates_narrow_files_byte_identically() {
    let schema = generic_i64_parent_schema("typed_compile_narrow", &[100, 0, 2]).unwrap();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: vec![vec![1_000, 10, 12], vec![2_000, 11, 14]],
        stream_id: 2,
        dictionary_id: 4,
        header_comment: Some("narrow".to_owned()),
    })
    .unwrap();
    assert_eq!(
        records::compile_i64_file(&ingest, Profile::Aura0).unwrap(),
        records::compile_typed_file(&ingest, Profile::Aura0).unwrap()
    );
    assert_eq!(
        records::compile_i64_file(&ingest, Profile::Aura1).unwrap(),
        records::compile_typed_file(&ingest, Profile::Aura1).unwrap()
    );
}

#[test]
fn unsigned_maxima_beside_opaque16_use_exact_wider_signed_lanes() {
    let schema = SchemaBuilder::new("typed_unsigned_maxima")
        .field("u8_value", FieldType::U8, FieldRole::Value)
        .field("u16_value", FieldType::U16, FieldRole::Value)
        .field("u32_value", FieldType::U32, FieldRole::Value)
        .field("exec_id", FieldType::Opaque16, FieldRole::Identifier)
        .finish()
        .unwrap();
    let rows = vec![
        vec![
            AuraTypedValue::I64(255),
            AuraTypedValue::I64(65_535),
            AuraTypedValue::I64(4_000_000_000),
            AuraTypedValue::Opaque16([0xFF; 16]),
        ],
        vec![
            AuraTypedValue::I64(0),
            AuraTypedValue::I64(1),
            AuraTypedValue::I64(u32::MAX.into()),
            AuraTypedValue::Opaque16([0x01; 16]),
        ],
    ];
    let ingest = records::encode_ingest_typed_file(records::TypedFileInput {
        schema,
        rows: rows.clone(),
        stream_id: 5,
        dictionary_id: 6,
        header_comment: Some("unsigned maxima".to_owned()),
    })
    .unwrap();
    let aura0 = records::compile_typed_file(&ingest, Profile::Aura0).unwrap();
    let aura1 = records::compile_typed_file(&aura0, Profile::Aura1).unwrap();
    let decoded_aura1 = records::decode_typed_file(&aura1).unwrap();
    assert_eq!(rows, decoded_aura1.rows);
    assert_eq!(rows, AuraTypedReader::open(&aura1).unwrap().rows());

    let plan = decoded_aura1
        .compiled_footer
        .unwrap()
        .aura1_program
        .to_aura1_plan(1)
        .unwrap();
    assert_eq!(PhysicalWidth::I16, plan.fields[0].width);
    assert_eq!(PhysicalWidth::I32, plan.fields[1].width);
    assert_eq!(PhysicalWidth::I64, plan.fields[2].width);
    assert_eq!(PhysicalWidth::I128, plan.fields[3].width);

    let aura0_again = records::compile_typed_file(&aura1, Profile::Aura0).unwrap();
    assert_eq!(rows, records::decode_typed_file(&aura0_again).unwrap().rows);
}

#[test]
fn typed_writer_reports_slot_row_and_upgrade_for_overflow() {
    let schema = generic_i64_parent_schema("typed_overflow", &[100, 0]).unwrap();
    let mut writer = AuraTypedWriter::new(schema);
    let result = writer.push_row(vec![
        AuraTypedValue::I64(1_000),
        AuraTypedValue::I128(i128::from(i64::MAX) + 1),
    ]);
    let diagnostic = diagnostic(result.unwrap_err());

    assert_eq!("width mismatch", diagnostic.reason);
    assert_eq!(Some(0), diagnostic.row_index);
    assert_eq!(Some(1), diagnostic.slot_index);
    assert_eq!("i64", diagnostic.declared_type);
    assert_eq!("i128", diagnostic.observed_type);
    assert_eq!("wide integer", diagnostic.observed_value_class);
    assert_eq!(Some("i128"), diagnostic.suggested_upgrade);
}

#[test]
fn derived_slot_rejects_double_population() {
    let schema = generic_i64_parent_schema("typed_internal_derivation", &[100, 0, 2]).unwrap();
    let mut writer = AuraTypedWriter::new(schema);
    writer.mark_internal_derivation(2).unwrap();

    let result = writer.push_row(vec![
        AuraTypedValue::I64(1_000),
        AuraTypedValue::I64(10),
        AuraTypedValue::I64(12),
    ]);
    let diagnostic = diagnostic(result.unwrap_err());

    assert_eq!("derived slot source conflict", diagnostic.reason);
    assert_eq!(Some(0), diagnostic.row_index);
    assert_eq!(Some(2), diagnostic.slot_index);
    assert_eq!(
        Some("remove supplied value or disable internal derivation"),
        diagnostic.suggested_upgrade
    );
}

#[test]
fn internally_derived_expression_is_materialized_from_short_rows() {
    let expression = DerivedExpression::new(3, 3, DerivedExpressionOp::Mul, vec![1, 2])
        .unwrap()
        .with_source(DerivedExpressionSource::Internal)
        .unwrap();
    let schema = generic_i64_parent_schema("typed_internal_expression", &[100, 0, 0, 103])
        .unwrap()
        .with_derived_expressions(vec![expression])
        .unwrap();
    let mut writer = AuraTypedWriter::new(schema);
    writer
        .extend_rows([
            vec![1_000.into(), 10.into(), 20.into()],
            vec![2_000.into(), 11.into(), 30.into()],
        ])
        .unwrap();

    let ingest = writer.finish().unwrap();
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let rows = vec![vec![1_000, 10, 20, 200], vec![2_000, 11, 30, 330]];

    assert_eq!(rows, records::decode_i64_file(&ingest).unwrap().rows);
    assert_eq!(rows, records::decode_i64_file(&aura0).unwrap().rows);
}

#[test]
fn externally_supplied_derived_slot_roundtrips_as_logical_field() {
    let schema = generic_i64_parent_schema("typed_external_derivation", &[100, 0, 2, 2]).unwrap();
    assert_eq!(FieldRelation::DeltaFromField(1), schema.fields[2].relation);
    let mut writer = AuraTypedWriter::new(schema);
    writer
        .extend_rows([
            vec![1_000.into(), 10.into(), 12.into(), 8.into()],
            vec![2_000.into(), 11.into(), 14.into(), 9.into()],
        ])
        .unwrap();

    let ingest = writer.finish().unwrap();
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let aura1 = records::compile_i64_file(&ingest, Profile::Aura1).unwrap();
    let rows = vec![vec![1_000, 10, 12, 8], vec![2_000, 11, 14, 9]];

    assert_eq!(rows, records::decode_i64_file(&ingest).unwrap().rows);
    assert_eq!(rows, records::decode_i64_file(&aura0).unwrap().rows);
    assert_eq!(rows, records::decode_i64_file(&aura1).unwrap().rows);
}

#[test]
fn opaque_16_fields_are_not_numeric_delta_fields_by_default() {
    let schema = typed_wide_schema();

    assert_eq!(FieldType::Opaque16, schema.fields[1].field_type);
    assert_eq!(FieldRelation::None, schema.fields[1].relation);
    assert!(schema.fields[1]
        .candidates
        .contains(FieldTransform::Absolute));
    assert!(!schema.fields[1]
        .candidates
        .contains(FieldTransform::DeltaPrevious));
    assert!(!schema.fields[1]
        .candidates
        .contains(FieldTransform::DeltaRelated));

    let invalid = SchemaBuilder::new("bad_opaque_relation")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("parent", FieldType::I64, FieldRole::Value)
        .field_related_to(
            "opaque",
            FieldType::Opaque16,
            FieldRole::Identifier,
            "parent",
        )
        .finish();
    assert!(invalid.is_err());
}
