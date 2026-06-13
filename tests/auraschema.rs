use aura_codec::schema::{AuraSchemaDefinition, FieldRelation};
use aura_codec::AuraError;

#[test]
fn auraschema_parses_header_comment_and_parent_map() {
    let definition = AuraSchemaDefinition::parse(
        r#"
        // reusable schema relationship definition
        "open_time_ms,open,high,low,close,volume,close_time_ms,quote_asset_volume,number_of_trades,taker_buy_base_asset_volume,taker_buy_quote_asset_volume"

        255 0 2 2 2 0
        1 0 0 6 8 // close_time->open_time, taker fields->totals
        "#,
    )
    .unwrap();

    assert_eq!(
        "open_time_ms,open,high,low,close,volume,close_time_ms,quote_asset_volume,number_of_trades,taker_buy_base_asset_volume,taker_buy_quote_asset_volume",
        definition.comment
    );
    assert_eq!(
        &[255, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8],
        definition.schema_mapping.as_slice()
    );

    let schema = definition
        .generic_i64_schema("binance_spot_kline_v1")
        .unwrap();
    assert_eq!(11, schema.fields.len());
    assert_eq!(FieldRelation::DeltaFromField(1), schema.fields[2].relation);
    assert_eq!(FieldRelation::DeltaFromField(0), schema.fields[6].relation);
    assert_eq!(FieldRelation::DeltaFromField(5), schema.fields[9].relation);
    assert_eq!(FieldRelation::DeltaFromField(7), schema.fields[10].relation);
}

#[test]
fn auraschema_requires_quoted_inserted_comment() {
    let result = AuraSchemaDefinition::parse(
        r#"
        ts,open,high,low,close,volume
        255 0 2 2 2 0
        "#,
    );

    assert_eq!(Err(AuraError::InvalidValue("auraschema comment")), result);
}

#[test]
fn auraschema_rejects_forward_parent_bytes() {
    let result = AuraSchemaDefinition::parse(
        r#"
        "ts,open,high"
        255 0 4
        "#,
    );

    assert_eq!(Err(AuraError::InvalidValue("auraschema parent")), result);
}
