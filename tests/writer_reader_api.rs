use aura_codec::reader;
use aura_codec::records::{self, I64FileInput};
use aura_codec::schema::{generic_i64_parent_schema, ohlcv_schema};
use aura_codec::writer;
use aura_codec::{AuraI64Reader, AuraI64Writer, Profile};

fn ohlcv_rows() -> Vec<Vec<i64>> {
    vec![
        vec![1_000_000_000, 10_000, 10_100, 9_900, 10_050, 500],
        vec![61_000_000_000, 20_000, 20_120, 19_950, 20_060, 525],
        vec![121_000_000_000, 10_010, 10_150, 9_980, 10_030, 510],
    ]
}

fn sample_input() -> I64FileInput {
    I64FileInput {
        schema: ohlcv_schema().unwrap(),
        rows: ohlcv_rows(),
        stream_id: 12,
        dictionary_id: 44,
        header_comment: Some("ts,open,high,low,close,volume".to_owned()),
    }
}

#[test]
fn writer_i64_finish_matches_legacy_ingest_helper() {
    let input = sample_input();
    let legacy = records::encode_ingest_i64_file(input.clone()).unwrap();
    let facade = AuraI64Writer::from_input(input).finish().unwrap();

    assert_eq!(legacy, facade);
}

#[test]
fn reader_decodes_all_profiles_and_exposes_metadata() {
    let input = sample_input();
    let rows = input.rows.clone();
    let ingest = writer::encode_i64(input).unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let aura1 = AuraI64Writer::compile_profile(&ingest, Profile::Aura1).unwrap();

    for (file, profile) in [
        (&ingest, Profile::Ingest),
        (&aura0, Profile::Aura0),
        (&aura1, Profile::Aura1),
    ] {
        let reader = AuraI64Reader::open(file).unwrap();

        assert_eq!(profile, reader.profile());
        assert_eq!(12, reader.header().stream_id);
        assert_eq!(44, reader.header().dictionary_id);
        assert_eq!("ts,open,high,low,close,volume", reader.header().comment);
        assert_eq!(rows, reader.rows());
        assert_eq!(rows[0][0], reader.header().base_time_ns);
    }
}

#[test]
fn experimental_column_decode_matches_rows_without_row_materialization() {
    let input = sample_input();
    let rows = input.rows.clone();
    let ingest = writer::encode_i64(input).unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();

    let decoded = records::decode_i64_columns_file(&aura0)
        .unwrap()
        .expect("generic Aura0 columns");

    assert_eq!(Profile::Aura0, decoded.header.profile);
    assert_eq!(rows.len(), decoded.record_count);
    assert_eq!(rows[0].len(), decoded.columns.len());
    for (slot, column) in decoded.columns.iter().enumerate() {
        assert_eq!(
            rows.iter().map(|row| row[slot]).collect::<Vec<_>>(),
            *column
        );
    }
}

#[test]
fn experimental_aura1_row_visitor_matches_rows_without_returning_vectors() {
    let input = sample_input();
    let rows = input.rows.clone();
    let ingest = writer::encode_i64(input).unwrap();
    let aura1 = writer::compile_i64(&ingest, Profile::Aura1).unwrap();
    let mut visited = Vec::new();

    let count = records::visit_i64_rows_file(&aura1, |row| {
        visited.push(row.to_vec());
        Ok(())
    })
    .unwrap();

    assert_eq!(rows.len(), count);
    assert_eq!(rows, visited);
}

#[test]
fn writer_profile_output_preserves_rows_and_footer_stamps() {
    let input = I64FileInput {
        schema: generic_i64_parent_schema(
            "writer_stamp_flow",
            &[100, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8],
        )
        .unwrap(),
        rows: (0..48)
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
            .collect(),
        stream_id: 2,
        dictionary_id: 9,
        header_comment: None,
    };
    let rows = input.rows.clone();
    let ingest = AuraI64Writer::from_input(input).finish().unwrap();
    let ingest_plan = reader::decode_i64(&ingest)
        .unwrap()
        .ingest_footer
        .unwrap()
        .generic_aura0_plan
        .unwrap();

    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let decoded = reader::decode_i64(&aura0).unwrap();
    let compiled_plan = decoded
        .compiled_footer
        .as_ref()
        .unwrap()
        .generic_aura0_plan
        .clone()
        .unwrap();

    assert_eq!(rows, decoded.rows);
    assert_eq!(ingest_plan, compiled_plan);
}

#[test]
fn writer_header_schema_map_matches_current_emitted_dialect() {
    let parent_map = [100, 0, 2, 204, 4, 5, 5];
    let schema = generic_i64_parent_schema("writer_schema_map", &parent_map).unwrap();
    let mut writer = AuraI64Writer::new(schema)
        .with_stream(1, 1)
        .with_header_comment("ts,a,b,side,price,qty_a,qty_b");
    writer
        .extend_rows([
            vec![1_000, 10, 20, 0, 100_000, 5, 0],
            vec![1_000, 10, 20, 0, 100_010, 0, 1],
        ])
        .unwrap();
    let file = writer.finish().unwrap();

    let decoded = reader::decode_i64(&file).unwrap();
    assert_eq!(parent_map, decoded.header.schema_mapping.as_slice());
    assert_eq!(Profile::Ingest, decoded.header.profile);
}
