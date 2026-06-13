use aura_codec::schema::ohlcv_schema;
use aura_codec::{records, AuraError, FieldEncoding, Profile};

fn sample_ohlcv_rows() -> Vec<Vec<i64>> {
    vec![
        vec![1_000_000_000, 10_000, 10_100, 9_900, 10_050, 500],
        vec![61_000_000_000, 20_000, 20_100, 19_900, 20_050, 525],
        vec![121_000_000_000, 10_000, 10_100, 9_900, 10_050, 510],
    ]
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn trailer_footer_len(file: &[u8]) -> u32 {
    let seal_offset = file.len() - 8;
    read_u32_le(&file[seal_offset - 4..seal_offset])
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
    assert_eq!(b"sealed:)", &file[file.len() - 8..]);
    let footer_end = decoded.header.footer_offset as usize + trailer_footer_len(&file) as usize;
    assert_eq!(file.len() - 12, footer_end);
    assert_eq!(1, decoded.header.stream_id);
    assert_eq!(7, decoded.header.dictionary_id);
    assert_eq!(1_000_000_000, decoded.header.base_time_ns);
    assert_eq!(0, decoded.header.schema_hash);
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
fn i64_file_trailer_stores_footer_length_before_seal() {
    let schema = ohlcv_schema().unwrap();
    let rows = sample_ohlcv_rows();
    let file = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 1,
        dictionary_id: 7,
    })
    .unwrap();

    let seal_offset = file.len() - 8;
    let footer_len_offset = seal_offset - 4;
    let decoded = records::decode_i64_file(&file).unwrap();

    assert_eq!(b"sealed:)", &file[seal_offset..]);
    assert_eq!(
        (footer_len_offset - decoded.header.footer_offset as usize) as u32,
        read_u32_le(&file[footer_len_offset..seal_offset])
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
    assert!(trailer_footer_len(&aura0) < 256);
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
    assert!(trailer_footer_len(&aura0) < 256);
}

#[test]
fn i64_file_decode_requires_trailing_seal_magic() {
    let schema = ohlcv_schema().unwrap();
    let rows = sample_ohlcv_rows();
    let mut file = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 1,
        dictionary_id: 7,
    })
    .unwrap();
    file.truncate(file.len() - 8);

    assert_eq!(
        Err(AuraError::InvalidMagic {
            expected: "sealed:)"
        }),
        records::decode_i64_file(&file)
    );
}
