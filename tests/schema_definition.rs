use aura_codec::schema::{FieldRelation, I64SchemaDefinition};
use aura_codec::{records, Profile};

const FIELD_NAMES: &[&str] = &[
    "open_time_ns",
    "open",
    "high",
    "low",
    "close",
    "volume",
    "close_time_ns",
    "quote_asset_volume",
    "number_of_trades",
    "taker_buy_base_asset_volume",
    "taker_buy_quote_asset_volume",
];

const PARENT_SLOTS: &[u8] = &[100, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8];

#[test]
fn code_defined_i64_schema_definition_supplies_schema_and_header_comment() {
    let definition = I64SchemaDefinition::from_field_names(
        "binance_spot_kline_i64_v1",
        FIELD_NAMES,
        PARENT_SLOTS,
    )
    .unwrap();

    assert_eq!(FIELD_NAMES.join(","), definition.header_comment());
    assert_eq!(PARENT_SLOTS, definition.parent_slots());

    let schema = definition.schema();
    assert_eq!(11, schema.fields.len());
    assert_eq!(FieldRelation::DeltaFromField(1), schema.fields[2].relation);
    assert_eq!(FieldRelation::DeltaFromField(0), schema.fields[6].relation);
    assert_eq!(FieldRelation::DeltaFromField(5), schema.fields[9].relation);
    assert_eq!(FieldRelation::DeltaFromField(7), schema.fields[10].relation);

    let file = records::encode_ingest_i64_file(records::I64FileInput {
        schema: schema.clone(),
        rows: vec![vec![
            0, 10_000, 10_050, 9_970, 10_020, 1_000, 59_999, 10_000_000, 10, 400, 4_000_000,
        ]],
        stream_id: 1,
        dictionary_id: 2,
        header_comment: Some(definition.header_comment().to_owned()),
    })
    .unwrap();

    let decoded = records::decode_i64_file(&file).unwrap();
    assert_eq!(Profile::Ingest, decoded.header.profile);
    assert_eq!(FIELD_NAMES.join(","), decoded.header.comment);
    assert_eq!(PARENT_SLOTS, decoded.header.schema_mapping.as_slice());
}

#[test]
fn code_defined_i64_schema_definition_validates_header_shape() {
    assert!(I64SchemaDefinition::from_field_names("bad", &["ts"], &[]).is_err());
    assert!(I64SchemaDefinition::from_field_names("bad", &["ts", "v1"], &[100, 2]).is_err());
    assert!(I64SchemaDefinition::from_field_names("bad", &["ts"], &[100, 0]).is_err());
}
