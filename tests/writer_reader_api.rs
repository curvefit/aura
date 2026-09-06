use aura_codec::reader;
use aura_codec::records::{
    self, Aura0ByteLaneCodec, Aura0ByteLaneUse, Aura0DecodePath, Aura0EncoderPath,
    Aura0FileProfile, I64FileInput, OutputGuardMode, TranscodePath,
};
use aura_codec::schema::{generic_i64_parent_schema, ohlcv_schema};
use aura_codec::writer;
use aura_codec::{
    Aura0ColumnPath, Aura1BodyPath, Aura1ExecutionOptions, AuraHeader, AuraI64Reader,
    AuraI64Writer, DerivedExpression, DerivedExpressionOp, Profile, UnsupportedPathBehavior,
};

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

fn bytes_guard(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |acc, byte| {
        acc.wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(*byte))
    })
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
fn aura0_to_aura1_fused_output_guard_matches_full_scan() {
    let ingest = writer::encode_i64(sample_input()).unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let expected = writer::compile_i64(&aura0, Profile::Aura1).unwrap();
    let guarded = records::try_compile_i64_file_with_fused_output_guard(&aura0, Profile::Aura1)
        .unwrap()
        .expect("Aura0 -> Aura1 fused guard");

    assert_eq!(expected, guarded.bytes);
    assert_eq!(bytes_guard(&expected), guarded.guard);
}

#[test]
fn aura0_to_aura1_default_fast_path_matches_column_fallback() {
    let input = sample_input();
    let rows = input.rows.clone();
    let ingest = writer::encode_i64(input).unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let direct = writer::compile_i64(&aura0, Profile::Aura1).unwrap();

    let columns = records::try_compile_i64_file_profiled_with_options(
        &aura0,
        Profile::Aura1,
        OutputGuardMode::NoGuard,
        TranscodePath::Auto,
        Aura1ExecutionOptions {
            body_path: Aura1BodyPath::Columns,
            column_path: Aura0ColumnPath::Materialized,
            unsupported_path: UnsupportedPathBehavior::Error,
        },
    )
    .unwrap()
    .expect("column body path")
    .profiled
    .bytes;

    assert_eq!(direct, columns);
    let decoded = reader::decode_i64(&direct).unwrap();
    assert_eq!(Profile::Aura1, decoded.header.profile);
    assert_eq!(rows, decoded.rows);
}

#[test]
fn aura0_to_aura1_compiled_path_reconstructs_temporal_derived_rows_directly() {
    let expressions = vec![
        DerivedExpression::new(1, 1, DerivedExpressionOp::FirstOffsetThenDelta, vec![4]).unwrap(),
        DerivedExpression::new(2, 2, DerivedExpressionOp::MaxPlusResidual, vec![1, 4]).unwrap(),
        DerivedExpression::new(3, 3, DerivedExpressionOp::MinMinusResidual, vec![1, 4]).unwrap(),
    ];
    let schema = generic_i64_parent_schema("derived_ohlcv", &[100, 101, 102, 103, 2, 0])
        .unwrap()
        .with_derived_expressions(expressions)
        .unwrap();
    let rows = (0..1_024i64)
        .scan(100_000i64, |previous_close, index| {
            let open = *previous_close + index.rem_euclid(3) - 1;
            let close = open + index.rem_euclid(7) - 3;
            let high = open.max(close) + index.rem_euclid(2);
            let low = open.min(close) - index.rem_euclid(3);
            *previous_close = close;
            Some(vec![1_000 + index * 1_000, open, high, low, close, 10_000])
        })
        .collect::<Vec<_>>();
    let ingest = writer::encode_i64(I64FileInput {
        schema,
        rows,
        stream_id: 7,
        dictionary_id: 11,
        header_comment: None,
    })
    .unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();

    let expected = writer::compile_i64(&aura0, Profile::Aura1).unwrap();
    let compiled = records::try_compile_i64_file_profiled(
        &aura0,
        Profile::Aura1,
        OutputGuardMode::NoGuard,
        TranscodePath::Auto,
        Aura0EncoderPath::Materialized,
        Aura0DecodePath::Materialized,
    )
    .unwrap()
    .expect("compiled derived Aura0 -> Aura1 path");

    assert_eq!(expected, compiled.bytes);
    assert!(compiled.conversion_plan_hash.is_some());
    let stats = compiled.stats.aura0_to_aura1.unwrap();
    assert!(stats.writer.temporary_buffer_bytes < 1_024 * 6 * size_of::<i64>());

    let guarded = records::try_compile_i64_file_with_fused_output_guard(&aura0, Profile::Aura1)
        .unwrap()
        .expect("compiled derived path with fused guard");
    assert_eq!(expected, guarded.bytes);
    assert_eq!(bytes_guard(&expected), guarded.guard);
}

#[test]
fn aura0_fast_lz4_byte_lane_round_trips_to_aura1_bytes() {
    let ingest = writer::encode_i64(sample_input()).unwrap();
    let aura1 = writer::compile_i64(&ingest, Profile::Aura1).unwrap();
    for codec in [Aura0ByteLaneCodec::Lz4, Aura0ByteLaneCodec::Zstd3] {
        let fast =
            records::compile_i64_file_with_aura0_profile(&aura1, Aura0FileProfile::Fast, codec)
                .unwrap();

        let expanded =
            records::compile_aura0_to_aura1_bytes_with_lane(&fast, Aura0ByteLaneUse::Always, true)
                .unwrap();

        assert_eq!(aura1, expanded);
        assert_eq!(
            reader::decode_i64(&aura1).unwrap().rows,
            reader::decode_i64(&expanded).unwrap().rows
        );
    }
}

#[test]
fn aura0_hybrid_lz4_byte_lane_keeps_semantic_fallback() {
    let ingest = writer::encode_i64(sample_input()).unwrap();
    let aura1 = writer::compile_i64(&ingest, Profile::Aura1).unwrap();
    let hybrid = records::compile_i64_file_with_aura0_profile(
        &aura1,
        Aura0FileProfile::Hybrid,
        Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();

    let byte_lane =
        records::compile_aura0_to_aura1_bytes_with_lane(&hybrid, Aura0ByteLaneUse::Always, true)
            .unwrap();
    let semantic =
        records::compile_aura0_to_aura1_bytes_with_lane(&hybrid, Aura0ByteLaneUse::Never, false)
            .unwrap();

    assert_eq!(aura1, byte_lane);
    assert_eq!(
        reader::decode_i64(&aura1).unwrap().rows,
        reader::decode_i64(&semantic).unwrap().rows
    );
}

#[test]
fn forcing_byte_lane_on_compact_aura0_fails_clearly() {
    let ingest = writer::encode_i64(sample_input()).unwrap();
    let compact = writer::compile_i64(&ingest, Profile::Aura0).unwrap();

    let err =
        records::compile_aura0_to_aura1_bytes_with_lane(&compact, Aura0ByteLaneUse::Always, false)
            .unwrap_err();

    assert!(err.to_string().contains("byte lane"));
}

#[test]
fn corrupt_aura0_byte_lane_payload_rejects_in_verify_mode() {
    let ingest = writer::encode_i64(sample_input()).unwrap();
    let aura1 = writer::compile_i64(&ingest, Profile::Aura1).unwrap();
    let mut fast = records::compile_i64_file_with_aura0_profile(
        &aura1,
        Aura0FileProfile::Fast,
        Aura0ByteLaneCodec::Raw,
    )
    .unwrap();
    let header_len = AuraHeader::encoded_len(&fast).unwrap();
    fast[header_len + 8] ^= 0x7f;

    let err =
        records::compile_aura0_to_aura1_bytes_with_lane(&fast, Aura0ByteLaneUse::Always, true)
            .unwrap_err();

    assert!(err.to_string().contains("byte lane checksum"));
}

#[test]
fn unsupported_aura0_byte_lane_codec_rejects() {
    let ingest = writer::encode_i64(sample_input()).unwrap();
    let aura1 = writer::compile_i64(&ingest, Profile::Aura1).unwrap();
    let mut fast = records::compile_i64_file_with_aura0_profile(
        &aura1,
        Aura0FileProfile::Fast,
        Aura0ByteLaneCodec::Raw,
    )
    .unwrap();
    let footer_extension = fast
        .windows(4)
        .position(|window| window == b"AUBL")
        .expect("byte-lane footer extension");
    let codec_id_offset = footer_extension + 4 + 4 + 1;
    fast[codec_id_offset] = 99;

    let err =
        records::compile_aura0_to_aura1_bytes_with_lane(&fast, Aura0ByteLaneUse::Always, false)
            .unwrap_err();

    assert!(err.to_string().contains("byte lane codec"));
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
#[test]
fn signed_minimum_values_remain_encodable_in_explicit_events() {
    use aura_codec::generic_planner::I64SearchEffort;
    use aura_codec::{generic_i64_parent_schema, AuraI64EventReader, AuraI64EventWriter, I64Event};
    for values in [
        vec![i64::MIN, i64::MIN],
        vec![0, i64::MIN],
        vec![i64::MIN, 0, i64::MIN],
    ] {
        for effort in [I64SearchEffort::Full, I64SearchEffort::Bounded] {
            let schema = generic_i64_parent_schema(
                "signed-minimum-events",
                &[100, 0, 200, 205, 0, 0, 5, 0, 0],
            )
            .unwrap();
            let events = values
                .iter()
                .copied()
                .map(|value| I64Event {
                    event_values: vec![1000, 1, value],
                    children: vec![vec![0, 100, 20, 19, 1]],
                })
                .collect::<Vec<_>>();
            let mut writer = AuraI64EventWriter::new(schema);
            for event in &events {
                writer.push_event(event.clone()).unwrap();
            }
            let bytes = writer.finish_aura0_with_search(effort).unwrap();
            let public = AuraI64EventReader::open(&bytes).unwrap();
            let independent = aura_codec::records::decode_i64_events_file(&bytes).unwrap();
            assert_eq!(public.events(), events);
            assert_eq!(independent.events, events);
        }
    }
}
