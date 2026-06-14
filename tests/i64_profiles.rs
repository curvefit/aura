use aura_codec::format::FORMAT_VERSION;
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

fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

fn trailer_footer_len(file: &[u8]) -> u32 {
    let seal_offset = file.len() - 8;
    read_u32_le(&file[seal_offset - 4..seal_offset])
}

fn trailer_footer_start(file: &[u8]) -> usize {
    let seal_offset = file.len() - 8;
    let footer_len_offset = seal_offset - 4;
    footer_len_offset - trailer_footer_len(file) as usize
}

fn trailer_footer_bytes(file: &[u8]) -> &[u8] {
    let footer_start = trailer_footer_start(file);
    let footer_end = file.len() - 12;
    &file[footer_start..footer_end]
}

#[test]
fn ingest_header_stores_parent_mapping_before_body() {
    let schema = ohlcv_schema().unwrap();
    let rows = sample_ohlcv_rows();
    let file = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 1,
        dictionary_id: 7,
        header_comment: None,
    })
    .unwrap();

    let header_len = usize::from(file[7]);

    assert_eq!(b"AURA", &file[..4]);
    assert_eq!(FORMAT_VERSION.to_le_bytes(), file[4..6]);
    assert_eq!(Profile::Ingest as u8, file[6]);
    assert_eq!(28, header_len);
    assert_eq!(1_000_000_000i64.to_le_bytes(), file[8..16]);
    assert_eq!(1u16.to_le_bytes(), file[16..18]);
    assert_eq!(7u16.to_le_bytes(), file[18..20]);
    assert_eq!(6, file[20]);
    assert_eq!(0, file[21]);
    assert_eq!(&[255, 0, 2, 2, 2, 0], &file[22..28]);
    assert_eq!(3, read_u64_le(&file[header_len..header_len + 8]));
}

#[test]
fn ingest_header_stores_comment_after_parent_mapping() {
    let schema = ohlcv_schema().unwrap();
    let rows = sample_ohlcv_rows();
    let comment = "ts,open,high,low,close,volume";
    let file = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 1,
        dictionary_id: 7,
        header_comment: Some(comment.to_string()),
    })
    .unwrap();

    let header_len = usize::from(file[7]);

    assert_eq!(57, header_len);
    assert_eq!(6, file[20]);
    assert_eq!(29, file[21]);
    assert_eq!(&[255, 0, 2, 2, 2, 0], &file[22..28]);
    assert_eq!(comment.as_bytes(), &file[28..57]);
    assert_eq!(3, read_u64_le(&file[header_len..header_len + 8]));

    let decoded = records::decode_i64_file(&file).unwrap();
    assert_eq!(comment, decoded.header.comment);

    let aura0 = records::compile_i64_file(&file, Profile::Aura0).unwrap();
    let decoded_aura0 = records::decode_i64_file(&aura0).unwrap();
    assert_eq!(comment, decoded_aura0.header.comment);
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
        header_comment: None,
    })
    .unwrap();

    let decoded = records::decode_i64_file(&file).unwrap();

    assert_eq!(Profile::Ingest, decoded.header.profile);
    assert_eq!(b"sealed:)", &file[file.len() - 8..]);
    assert_eq!(
        file.len() - 12,
        trailer_footer_start(&file) + trailer_footer_len(&file) as usize
    );
    assert_eq!(1, decoded.header.stream_id);
    assert_eq!(7, decoded.header.dictionary_id);
    assert_eq!(1_000_000_000, decoded.header.base_time_ns);
    assert_eq!(
        &[255, 0, 2, 2, 2, 0],
        decoded.header.schema_mapping.as_slice()
    );
    assert_eq!(rows, decoded.rows);
    let footer = decoded.ingest_footer.as_ref().unwrap();
    assert_eq!(3, footer.stats.record_count);

    let plan = footer.aura0_plan.as_ref().unwrap();
    assert_eq!(
        FieldEncoding::ImplicitFixedStep,
        plan.field("ts_open", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::DerivedOffset,
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
        header_comment: None,
    })
    .unwrap();

    let seal_offset = file.len() - 8;
    let footer_len_offset = seal_offset - 4;

    assert_eq!(b"sealed:)", &file[seal_offset..]);
    assert_eq!(
        (footer_len_offset - trailer_footer_start(&file)) as u32,
        read_u32_le(&file[footer_len_offset..seal_offset])
    );
    records::decode_i64_file(&file).unwrap();
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
        header_comment: None,
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
fn compiled_aura0_uses_bitpacked_delta_body() {
    let schema = ohlcv_schema().unwrap();
    let rows: Vec<Vec<i64>> = (0..130)
        .map(|idx| {
            let ts = 1_000_000_000 + i64::from(idx) * 60_000_000_000;
            let open = 10_000 + i64::from(idx % 2);
            vec![ts, open, open + 1, open - 1, open, 500 + i64::from(idx % 2)]
        })
        .collect();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema: schema.clone(),
        rows: rows.clone(),
        stream_id: 2,
        dictionary_id: 9,
        header_comment: None,
    })
    .unwrap();

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let decoded = records::decode_i64_file(&aura0).unwrap();
    let plan = decoded
        .compiled_footer
        .as_ref()
        .unwrap()
        .aura0_program
        .to_aura0_plan()
        .unwrap();

    assert_eq!(rows, decoded.rows);
    assert_eq!(
        FieldEncoding::BitpackedDeltaBase,
        plan.field("open", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::DerivedOffset,
        plan.field("high", &schema).unwrap().encoding
    );
    assert!(aura0.len() < ingest.len() / 4);
}

#[test]
fn compiled_aura0_round_trips_derived_and_biased_bitpacked_fields() {
    let schema =
        aura_codec::generic_i64_parent_schema("derived_and_biased", &[255, 0, 2, 0]).unwrap();
    let mut previous = 10_000;
    let mut rows = Vec::new();
    for idx in 0..130 {
        if idx > 0 {
            previous += 1_000 + i64::from(idx - 1);
        }
        let parent = if idx % 2 == 0 { 1_000_000 } else { 2_000_000 };
        let related_delta = -130 + i64::from(idx);
        rows.push(vec![
            i64::from(idx),
            parent,
            parent + related_delta,
            previous,
        ]);
    }
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema: schema.clone(),
        rows: rows.clone(),
        stream_id: 2,
        dictionary_id: 9,
        header_comment: None,
    })
    .unwrap();

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let decoded = records::decode_i64_file(&aura0).unwrap();
    let plan = decoded
        .compiled_footer
        .as_ref()
        .unwrap()
        .aura0_program
        .to_aura0_plan()
        .unwrap();

    assert_eq!(rows, decoded.rows);
    assert_eq!(
        FieldEncoding::BitpackedDeltaRelatedOffset,
        plan.field("v2", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::BitpackedDeltaPreviousOffset,
        plan.field("v3", &schema).unwrap().encoding
    );
}

#[test]
fn compiled_aura0_uses_candle_and_residual_footer_programs() {
    let schema =
        aura_codec::generic_i64_parent_schema("btc_like", &[255, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8])
            .unwrap();
    let rows = vec![
        vec![
            0, 10_000, 10_050, 9_970, 10_020, 1_000, 59_999, 10_000_000, 10, 400, 4_000_000,
        ],
        vec![
            60_000, 10_025, 10_100, 9_990, 10_080, 1_200, 119_999, 12_036_000, 12, 500, 5_015_000,
        ],
        vec![
            120_000, 10_070, 10_090, 10_000, 10_010, 900, 179_999, 9_054_000, 9, 300, 3_018_000,
        ],
        vec![
            180_000, 10_015, 10_040, 9_960, 9_980, 1_500, 239_999, 15_022_500, 15, 700, 7_010_500,
        ],
    ];
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema: schema.clone(),
        rows: rows.clone(),
        stream_id: 2,
        dictionary_id: 9,
        header_comment: None,
    })
    .unwrap();

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let decoded = records::decode_i64_file(&aura0).unwrap();
    let plan = decoded
        .compiled_footer
        .as_ref()
        .unwrap()
        .aura0_program
        .to_aura0_plan()
        .unwrap();

    assert_eq!(rows, decoded.rows);
    assert_eq!(
        FieldEncoding::BitpackedDeltaPreviousFieldOffset,
        plan.field("v1", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::BitpackedCandleMaxOffset,
        plan.field("v2", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::BitpackedCandleMinOffset,
        plan.field("v3", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::BitpackedProductResidual,
        plan.field("v7", &schema).unwrap().encoding
    );
    assert_eq!(
        FieldEncoding::BitpackedProportionalResidual,
        plan.field("v10", &schema).unwrap().encoding
    );
}

#[test]
fn compiled_profiles_share_stamp_and_convert_without_replanning() {
    let schema = aura_codec::generic_i64_parent_schema(
        "aura0_to_aura1",
        &[255, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8],
    )
    .unwrap();
    let rows: Vec<Vec<i64>> = (0..32)
        .map(|idx| {
            let open = 10_000 + i64::from(idx % 5) * 10;
            let close = open + i64::from(idx % 7) - 3;
            let high = open.max(close) + i64::from(idx % 4);
            let low = open.min(close) - i64::from(idx % 3);
            let volume = 1_000 + i64::from(idx * 10);
            let quote = volume * low + i64::from(idx % 11);
            let taker_base = volume / 3;
            let taker_quote = quote * taker_base / volume + i64::from(idx % 13);
            vec![
                i64::from(idx) * 60_000,
                open,
                high,
                low,
                close,
                volume,
                i64::from(idx) * 60_000 + 59_999,
                quote,
                i64::from(idx),
                taker_base,
                taker_quote,
            ]
        })
        .collect();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: 2,
        dictionary_id: 9,
        header_comment: None,
    })
    .unwrap();
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let aura1 = records::compile_i64_file(&ingest, Profile::Aura1).unwrap();
    assert_eq!(trailer_footer_bytes(&aura0), trailer_footer_bytes(&aura1));

    let aura1_from_aura0 = records::compile_i64_file(&aura0, Profile::Aura1).unwrap();
    assert_eq!(
        trailer_footer_bytes(&aura0),
        trailer_footer_bytes(&aura1_from_aura0)
    );
    let decoded = records::decode_i64_file(&aura1_from_aura0).unwrap();

    assert_eq!(Profile::Aura1, decoded.header.profile);
    assert_eq!(rows, decoded.rows);
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
        header_comment: None,
    })
    .unwrap();

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let decoded = records::decode_i64_file(&aura0).unwrap();
    let footer = decoded.compiled_footer.as_ref().unwrap();

    assert_eq!(rows.len() as u64, footer.record_count);
    assert_eq!(6, footer.aura0_program.fields.len());
    assert_eq!(6, footer.aura1_program.fields.len());
    assert!(footer.aura0_program.encoded_len().unwrap() <= 80);
    assert!(trailer_footer_len(&aura0) < 320);
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
        header_comment: None,
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
