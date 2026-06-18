use aura_codec::footer::AuraFooter;
use aura_codec::schema::{
    generic_i64_parent_schema, FieldRelation, FieldRole, FieldTransform, FieldType, SchemaBuilder,
};
use aura_codec::{
    records, AuraError, AuraTypedValue, AuraTypedWriter, DerivedExpression, DerivedExpressionOp,
    DerivedExpressionSource, IngestStats, Profile,
};

fn typed_wide_schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("typed_wide_v1")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("exec_id", FieldType::Opaque16, FieldRole::Identifier)
        .field("notional", FieldType::I128, FieldRole::Value)
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
fn wide_values_roundtrip_or_return_structured_profile_diagnostic() {
    let schema = typed_wide_schema();
    let mut writer = AuraTypedWriter::new(schema);
    writer
        .push_row(vec![
            AuraTypedValue::I64(1_000),
            AuraTypedValue::Opaque16([1; 16]),
            AuraTypedValue::I128(i128::from(i64::MAX) + 10),
        ])
        .unwrap();

    let diagnostic = diagnostic(writer.finish().unwrap_err());

    assert_eq!("unsupported profile", diagnostic.reason);
    assert_eq!(Some(1), diagnostic.slot_index);
    assert_eq!("opaque16", diagnostic.declared_type);
    assert_eq!("wide field", diagnostic.observed_value_class);
    assert_eq!(
        Some("wait for lossless wide-field body support"),
        diagnostic.suggested_upgrade
    );
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
