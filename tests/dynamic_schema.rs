use aura_codec::schema::{generic_i64_schema, FieldRelation, FieldTransform, RelatedFieldMapping};
use aura_codec::{records, AuraError, FieldEncoding, Profile};

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
        FieldEncoding::DeltaRelated,
        plan.field("v2", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::DeltaPrevious,
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
