use aura_codec::format::SEAL_MAGIC;
use aura_codec::schema::{
    generic_i64_parent_schema, FieldRole, FieldType, SchemaBuilder, SchemaDescriptor,
};
use aura_codec::writer;
use aura_codec::{records, AuraError, AuraTypedValue, AuraTypedWriter, Profile};

const PARENT_MAP: &[u8] = &[100, 0, 2, 2, 2, 0];

fn rows() -> Vec<Vec<i64>> {
    vec![
        vec![1_000_000_000, 10_000, 10_120, 9_950, 10_020, 500],
        vec![61_000_000_000, 10_010, 10_150, 9_980, 10_080, 550],
        vec![121_000_000_000, 10_050, 10_200, 10_000, 10_075, 525],
    ]
}

fn input(schema: SchemaDescriptor) -> records::I64FileInput {
    records::I64FileInput {
        schema,
        rows: rows(),
        stream_id: 77,
        dictionary_id: 19,
        header_comment: Some("ts,open,high,low,close,volume".to_owned()),
    }
}

fn diagnostic(error: AuraError) -> aura_codec::AuraDiagnostic {
    match error {
        AuraError::Diagnostic(diagnostic) => diagnostic,
        other => panic!("expected diagnostic error, got {other:?}"),
    }
}

#[test]
fn stamp_unstamped_ingest_rows_produces_full_aura_footer() {
    let schema = generic_i64_parent_schema("stamp_source", PARENT_MAP).unwrap();
    let file = writer::stamp_i64(input(schema.clone())).unwrap();
    let decoded = records::decode_i64_file(&file).unwrap();
    let footer = decoded.ingest_footer.as_ref().unwrap();

    assert_eq!(SEAL_MAGIC, &file[file.len() - SEAL_MAGIC.len()..]);
    assert_eq!(Profile::Ingest, decoded.header.profile);
    assert_eq!(77, decoded.header.stream_id);
    assert_eq!(19, decoded.header.dictionary_id);
    assert_eq!("ts,open,high,low,close,volume", decoded.header.comment);
    assert_eq!(PARENT_MAP, decoded.header.schema_mapping.as_slice());
    assert_eq!(rows(), decoded.rows);
    assert_eq!(schema.fields, footer.schema.fields);
    assert_eq!(rows().len() as u64, footer.stats.record_count);
    assert!(footer.aura0_plan.is_some());
    assert!(footer.aura1_plan.is_some());
    assert!(footer.generic_aura0_plan.is_some());
}

#[test]
fn restamp_preserves_rows_metadata_and_rejects_non_lossless_schema() {
    let source_schema = generic_i64_parent_schema("restamp_source", PARENT_MAP).unwrap();
    let source = writer::stamp_i64(input(source_schema)).unwrap();
    let flatter_schema =
        generic_i64_parent_schema("restamp_flatter", &[100, 0, 0, 0, 0, 0]).unwrap();
    let restamped = writer::restamp_i64(&source, flatter_schema).unwrap();
    let decoded = records::decode_i64_file(&restamped).unwrap();

    assert_eq!(Profile::Ingest, decoded.header.profile);
    assert_eq!(77, decoded.header.stream_id);
    assert_eq!(19, decoded.header.dictionary_id);
    assert_eq!("ts,open,high,low,close,volume", decoded.header.comment);
    assert_eq!(
        &[100, 0, 0, 0, 0, 0],
        decoded.header.schema_mapping.as_slice()
    );
    assert_eq!(rows(), decoded.rows);

    let short_schema = generic_i64_parent_schema("restamp_short", &[100, 0, 0]).unwrap();
    let short_error = diagnostic(writer::restamp_i64(&source, short_schema).unwrap_err());
    assert_eq!("field count mismatch", short_error.reason);
    assert_eq!(Some(0), short_error.row_index);

    let too_narrow = SchemaBuilder::new("restamp_too_narrow")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("open", FieldType::U8, FieldRole::Value)
        .field("high", FieldType::I64, FieldRole::Value)
        .field("low", FieldType::I64, FieldRole::Value)
        .field("close", FieldType::I64, FieldRole::Value)
        .field("volume", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let narrow_error = diagnostic(writer::restamp_i64(&source, too_narrow).unwrap_err());
    assert_eq!("overflow", narrow_error.reason);
    assert_eq!(Some(0), narrow_error.row_index);
    assert_eq!(Some(1), narrow_error.slot_index);
    assert_eq!("u8", narrow_error.declared_type);

    let wide_schema = SchemaBuilder::new("restamp_wide")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("exec_id", FieldType::Opaque16, FieldRole::Identifier)
        .field("notional", FieldType::I128, FieldRole::Value)
        .field("low", FieldType::I64, FieldRole::Value)
        .field("close", FieldType::I64, FieldRole::Value)
        .field("volume", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let wide_error = diagnostic(writer::restamp_i64(&source, wide_schema).unwrap_err());
    assert_eq!("unsupported profile", wide_error.reason);
    assert_eq!(Some(1), wide_error.slot_index);
    assert_eq!("opaque16", wide_error.declared_type);
}

#[test]
fn failed_finish_does_not_produce_fake_valid_aura_file() {
    let schema = SchemaBuilder::new("failed_finish")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("wide", FieldType::I128, FieldRole::Value)
        .finish()
        .unwrap();
    let mut writer = AuraTypedWriter::new(schema);
    writer
        .push_row(vec![
            AuraTypedValue::I64(1_000),
            AuraTypedValue::I128(i128::from(i64::MAX) + 1),
        ])
        .unwrap();

    let error = diagnostic(writer.finish().unwrap_err());
    assert_eq!("unsupported profile", error.reason);
    assert!(records::decode_i64_file(&[]).is_err());

    let valid = writer::stamp_i64(input(
        generic_i64_parent_schema("valid", PARENT_MAP).unwrap(),
    ))
    .unwrap();
    let mut missing_seal = valid.clone();
    missing_seal.truncate(missing_seal.len() - SEAL_MAGIC.len());
    assert!(records::decode_i64_file(&missing_seal).is_err());

    let mut partial_footer = valid;
    let seal_offset = partial_footer.len() - SEAL_MAGIC.len();
    partial_footer.drain(seal_offset - 8..seal_offset - 4);
    assert!(records::decode_i64_file(&partial_footer).is_err());
}
