use aura_codec::schema::ohlcv_schema;
use aura_codec::{records, FieldEncoding, Profile};

fn sample_ohlcv_rows() -> Vec<Vec<i64>> {
    vec![
        vec![1_000_000_000, 10_000, 10_100, 9_900, 10_050, 500],
        vec![61_000_000_000, 20_000, 20_100, 19_900, 20_050, 525],
        vec![121_000_000_000, 10_000, 10_100, 9_900, 10_050, 510],
    ]
}

#[test]
fn ingest_i64_file_round_trips_rows_and_footer_plans() {
    let schema = ohlcv_schema().unwrap();
    let rows = sample_ohlcv_rows();
    let file = records::encode_ingest_i64_file(records::I64FileInput {
        schema: schema.clone(),
        rows: rows.clone(),
        stream_id: 1,
        dictionary_id: 7,
    })
    .unwrap();

    let decoded = records::decode_i64_file(&file).unwrap();

    assert_eq!(Profile::Ingest, decoded.header.profile);
    assert!(decoded.header.is_sealed());
    assert_eq!(1, decoded.header.stream_id);
    assert_eq!(7, decoded.header.dictionary_id);
    assert_eq!(1_000_000_000, decoded.header.base_time_ns);
    assert_eq!(rows, decoded.rows);
    let footer = decoded.ingest_footer.as_ref().unwrap();
    assert_eq!(3, footer.stats.record_count);

    let plan = footer.aura0_plan.as_ref().unwrap();
    assert_eq!(
        FieldEncoding::ImplicitFixedStep,
        plan.field("ts_open", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::DeltaRelated,
        plan.field("high", &schema).unwrap().encoding
    );
}

#[test]
fn compiled_i64_profiles_round_trip_ingest_rows() {
    let schema = ohlcv_schema().unwrap();
    let rows = sample_ohlcv_rows();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: 2,
        dictionary_id: 9,
    })
    .unwrap();

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let aura1 = records::compile_i64_file(&ingest, Profile::Aura1).unwrap();

    let decoded_aura0 = records::decode_i64_file(&aura0).unwrap();
    let decoded_aura1 = records::decode_i64_file(&aura1).unwrap();

    assert_eq!(Profile::Aura0, decoded_aura0.header.profile);
    assert_eq!(Profile::Aura1, decoded_aura1.header.profile);
    assert!(decoded_aura0.ingest_footer.is_none());
    assert!(decoded_aura1.ingest_footer.is_none());
    assert!(decoded_aura0.compiled_footer.is_some());
    assert!(decoded_aura1.compiled_footer.is_some());
    assert_eq!(rows, decoded_aura0.rows);
    assert_eq!(rows, decoded_aura1.rows);
    assert!(aura0.len() < ingest.len());
    assert!(decoded_aura0.header.footer_len < 256);
}

#[test]
fn compiled_footer_omits_ingest_stats_and_uses_field_programs() {
    let schema = ohlcv_schema().unwrap();
    let rows = sample_ohlcv_rows();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: 2,
        dictionary_id: 9,
    })
    .unwrap();

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let decoded = records::decode_i64_file(&aura0).unwrap();
    let footer = decoded.compiled_footer.as_ref().unwrap();

    assert_eq!(rows.len() as u64, footer.record_count);
    assert_eq!(6, footer.program.fields.len());
    assert!(footer.program.encoded_len().unwrap() <= 48);
    assert!(decoded.header.footer_len < 256);
}
