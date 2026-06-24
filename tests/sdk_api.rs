use std::fs::{self, File};
use std::io::Cursor;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use aura_codec::{
    convert_aura, Aura0ByteLaneCodec, Aura0ByteLaneUse, AuraColumnBatch, AuraFormat, AuraProfile,
    AuraReader, AuraReaderSourceKind, AuraRecordBatch, AuraReplayBackend, AuraSchema, AuraType,
    AuraValue, AuraWriter, CompiledAuraPlan, ConvertOptions, GroupBy, ReaderOptions, WriterOptions,
};

fn market_schema() -> AuraSchema {
    AuraSchema::builder()
        .field("ts_event", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("price", AuraType::PriceI64Scaled { scale: 9 })
        .field("size", AuraType::U64)
        .field("side", AuraType::EnumU8)
        .field("flags", AuraType::FlagsU32)
        .build()
        .unwrap()
}

fn market_rows() -> Vec<Vec<AuraValue>> {
    vec![
        vec![
            1_700_000_000_000_000_000_i64.into(),
            10_u64.into(),
            101_000_000_000_i64.into(),
            25_u64.into(),
            1_u64.into(),
            0_u64.into(),
        ],
        vec![
            1_700_000_000_001_000_000_i64.into(),
            10_u64.into(),
            101_250_000_000_i64.into(),
            33_u64.into(),
            2_u64.into(),
            4_u64.into(),
        ],
        vec![
            1_700_000_000_002_000_000_i64.into(),
            22_u64.into(),
            99_500_000_000_i64.into(),
            10_u64.into(),
            1_u64.into(),
            1_u64.into(),
        ],
    ]
}

fn write_with_options(
    schema: AuraSchema,
    rows: Vec<Vec<AuraValue>>,
    options: WriterOptions,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    let batch = AuraRecordBatch::new(schema.clone(), rows).unwrap();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, options).unwrap();
    writer.write_batch(batch).unwrap();
    let summary = writer.finish().unwrap();
    assert_eq!(summary.output_bytes, bytes.len());
    bytes
}

fn assert_roundtrip(bytes: &[u8], schema: &AuraSchema, rows: &[Vec<AuraValue>]) {
    let reader = AuraReader::open(Cursor::new(bytes)).unwrap();
    assert_eq!(reader.schema().descriptor().name, schema.descriptor().name);
    assert_eq!(reader.schema().hash(), schema.hash());
    assert_eq!(reader.schema().field_count(), schema.field_count());
    for (actual, expected) in reader.schema().fields().iter().zip(schema.fields()) {
        assert_eq!(actual.name, expected.name);
        assert_eq!(actual.aura_type, expected.aura_type);
        assert_eq!(actual.nullable, expected.nullable);
    }
    let batches = reader.read_batches().unwrap();
    assert_eq!(1, batches.len());
    assert_eq!(rows, batches[0].rows());
}

fn write_temp_file(name: &str, bytes: &[u8]) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "aura_sdk_api_{name}_{}_{}.aura",
        std::process::id(),
        nanos
    ));
    fs::write(&path, bytes).unwrap();
    path
}

fn column_market_batch(schema: AuraSchema) -> AuraColumnBatch {
    AuraColumnBatch::builder(schema)
        .u32("flags", vec![0, 4, 1])
        .u64("size", vec![25, 33, 10])
        .u8("side", vec![1, 2, 1])
        .i64(
            "price",
            vec![101_000_000_000, 101_250_000_000, 99_500_000_000],
        )
        .u32("symbol_id", vec![10, 10, 22])
        .i64(
            "ts_event",
            vec![
                1_700_000_000_000_000_000,
                1_700_000_000_001_000_000,
                1_700_000_000_002_000_000,
            ],
        )
        .build()
        .unwrap()
}

#[test]
fn schema_builder_accepts_valid_market_data_schema() {
    let schema = market_schema();

    assert_eq!(6, schema.field_count());
    assert_eq!("ts_event", schema.fields()[0].name);
    assert_eq!(AuraType::U32, schema.fields()[1].aura_type);
    assert_eq!(
        AuraType::PriceI64Scaled { scale: 9 },
        schema.fields()[2].aura_type
    );
    assert_ne!(0, schema.hash());
}

#[test]
fn writer_reader_and_convert_options_defaults_are_stable() {
    let writer = WriterOptions::default();
    assert_eq!(AuraFormat::Aura0, writer.format);
    assert_eq!(AuraProfile::Compact, writer.aura0_profile);
    assert_eq!(Aura0ByteLaneCodec::Lz4, writer.byte_lane_codec);
    assert_eq!(0, writer.stream_id);
    assert_eq!(0, writer.dictionary_id);

    let aura1 = WriterOptions::aura1();
    assert_eq!(AuraFormat::Aura1, aura1.format);
    assert_eq!(AuraProfile::Compact, aura1.aura0_profile);

    let reader = ReaderOptions::default();
    assert_eq!(Aura0ByteLaneUse::Auto, reader.use_byte_lane);

    let convert = ConvertOptions::new(AuraFormat::Aura1);
    assert_eq!(AuraFormat::Aura1, convert.target_format);
    assert_eq!(AuraProfile::Compact, convert.aura0_profile);
    assert_eq!(Aura0ByteLaneUse::Auto, convert.use_byte_lane);
    assert!(!convert.verify);
}

#[test]
fn schema_builder_rejects_duplicate_field_names() {
    let err = AuraSchema::builder()
        .field("alpha", AuraType::I64)
        .field("alpha", AuraType::I64)
        .build()
        .unwrap_err();

    assert!(err.to_string().contains("duplicate field name"));
}

#[test]
fn schema_builder_rejects_duplicate_field_ids() {
    let err = AuraSchema::builder()
        .field_with_id(7, "alpha", AuraType::I64)
        .field_with_id(7, "beta", AuraType::I64)
        .build()
        .unwrap_err();

    assert!(err.to_string().contains("duplicate field id"));
}

#[test]
fn schema_builder_rejects_unsupported_type_and_nullability() {
    let unsupported = AuraSchema::builder()
        .field("payload", AuraType::Utf8)
        .build()
        .unwrap_err();
    assert!(unsupported.to_string().contains("unsupported aura type"));

    let nullable = AuraSchema::builder()
        .nullable_field("maybe_price", AuraType::I64)
        .build()
        .unwrap_err();
    assert!(nullable.to_string().contains("nullable field"));
}

#[test]
fn write_aura1_generic_schema_roundtrip() {
    let schema = market_schema();
    let rows = market_rows();
    let aura1 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura1());

    assert_roundtrip(&aura1, &schema, &rows);
}

#[test]
fn write_aura0_compact_generic_schema_roundtrip() {
    let schema = market_schema();
    let rows = market_rows();
    let aura0 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura0_compact());

    let mut reader = AuraReader::open(Cursor::new(&aura0)).unwrap();
    let open_stats = reader.stats();
    assert_eq!(0, open_stats.open_decoded_row_count);
    assert!(!open_stats.full_file_materialized);
    assert!(open_stats.streaming_reader_used);
    assert_eq!(
        rows.len(),
        reader.next_batch(16).unwrap().unwrap().row_count()
    );
    let batch_stats = reader.stats();
    assert!(!batch_stats.full_file_materialized);
    assert_eq!(rows.len(), batch_stats.rows_decoded_in_last_batch);
    assert_eq!(rows.len(), batch_stats.max_rows_materialized_at_once);

    assert_roundtrip(&aura0, &schema, &rows);
}

#[test]
fn reader_streaming_batches_aura0_fast_profile_without_full_row_materialization() {
    let schema = market_schema();
    let rows = market_rows();
    let aura0_fast = write_with_options(
        schema.clone(),
        rows.clone(),
        WriterOptions::aura0_compact().profile(AuraProfile::Fast),
    );

    let mut reader = AuraReader::open(Cursor::new(&aura0_fast)).unwrap();
    let open_stats = reader.stats();
    assert_eq!(0, open_stats.open_decoded_row_count);
    assert!(!open_stats.full_file_materialized);
    assert!(open_stats.streaming_reader_used);

    let first = reader.next_batch(2).unwrap().unwrap();
    assert_eq!(&rows[..2], first.rows());
    let first_stats = reader.stats();
    assert_eq!(2, first_stats.rows_decoded_in_last_batch);
    assert_eq!(2, first_stats.max_rows_materialized_at_once);
    assert!(!first_stats.full_file_materialized);

    let second = reader.next_batch(2).unwrap().unwrap();
    assert_eq!(&rows[2..], second.rows());
    assert!(reader.next_batch(2).unwrap().is_none());
}

#[test]
fn compiled_plan_public_from_schema_and_reader_footer() {
    let schema = market_schema();
    let plan = CompiledAuraPlan::from_schema(&schema).unwrap();

    assert_eq!(schema.hash(), plan.schema_hash);
    assert_eq!(6, plan.field_count);
    assert_eq!(33, plan.aura1_record_width);
    assert_eq!(0, plan.aura1_field_offset(0).unwrap().offset);
    assert_eq!(8, plan.aura1_field_offset(1).unwrap().offset);
    assert_eq!(12, plan.aura1_field_offset(2).unwrap().offset);
    assert_eq!(20, plan.aura1_field_offset(3).unwrap().offset);
    assert_eq!(28, plan.aura1_field_offset(4).unwrap().offset);
    assert_eq!(29, plan.aura1_field_offset(5).unwrap().offset);

    let rows = market_rows();
    let mut bytes = Vec::new();
    let batch = AuraRecordBatch::new(schema.clone(), rows).unwrap();
    let mut writer =
        AuraWriter::try_new(&mut bytes, schema.clone(), WriterOptions::aura1()).unwrap();
    assert_eq!(plan.schema_hash, writer.compiled_plan().schema_hash);
    writer.write_batch(batch).unwrap();
    writer.finish().unwrap();

    let reader = AuraReader::open(Cursor::new(&bytes)).unwrap();
    let read_plan = reader.compiled_plan().expect("compiled plan");
    assert_eq!(schema.hash(), read_plan.schema_hash);
    assert!(read_plan.aura1_record_width <= plan.aura1_record_width);
    assert_ne!(0, read_plan.conversion_plan_hash);
}

#[test]
fn schema_name_and_id_roundtrip_through_profiles_and_conversions() {
    let schema = AuraSchema::named("named_sdk_schema")
        .field("event_time", AuraType::TimestampNanos)
        .field("venue_code", AuraType::U16)
        .field("signed_qty", AuraType::I32)
        .build()
        .unwrap();
    let rows = vec![
        vec![10_i64.into(), 2_u64.into(), (-1_i64).into()],
        vec![11_i64.into(), 3_u64.into(), 9_i64.into()],
    ];
    let aura1 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura1());
    let aura0 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura0_compact());
    let mut converted = Vec::new();
    convert_aura(
        Cursor::new(&aura0),
        &mut converted,
        ConvertOptions::new(AuraFormat::Aura1).verify(true),
    )
    .unwrap();

    for bytes in [&aura1, &aura0, &converted] {
        let reader = AuraReader::open(Cursor::new(bytes)).unwrap();
        assert_eq!("named_sdk_schema", reader.schema().descriptor().name);
        assert_eq!(schema.hash(), reader.schema().hash());
        assert_eq!(schema.fields(), reader.schema().fields());
    }
}

#[test]
fn write_column_batch_roundtrips_and_matches_row_batch() {
    let schema = market_schema();
    let rows = market_rows();
    let row_bytes = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura1());
    let mut column_bytes = Vec::new();
    let mut writer =
        AuraWriter::try_new(&mut column_bytes, schema.clone(), WriterOptions::aura1()).unwrap();
    writer
        .write_batch(column_market_batch(schema.clone()))
        .unwrap();
    writer.finish().unwrap();

    assert_eq!(row_bytes, column_bytes);
    assert_roundtrip(&column_bytes, &schema, &rows);
}

#[test]
fn column_batch_rejects_length_missing_extra_and_type_mismatches() {
    let schema = market_schema();
    let length = AuraColumnBatch::builder(schema.clone())
        .i64("ts_event", vec![1, 2])
        .u32("symbol_id", vec![1])
        .i64("price", vec![10, 11])
        .u64("size", vec![1, 2])
        .u8("side", vec![1, 2])
        .u32("flags", vec![0, 0])
        .build()
        .unwrap_err();
    assert!(length.to_string().contains("column length"));

    let missing = AuraColumnBatch::builder(schema.clone())
        .i64("ts_event", vec![1])
        .u32("symbol_id", vec![1])
        .i64("price", vec![10])
        .u64("size", vec![1])
        .u8("side", vec![1])
        .build()
        .unwrap_err();
    assert!(missing.to_string().contains("missing column"));

    let extra = AuraColumnBatch::builder(schema.clone())
        .i64("ts_event", vec![1])
        .u32("symbol_id", vec![1])
        .i64("price", vec![10])
        .u64("size", vec![1])
        .u8("side", vec![1])
        .u32("flags", vec![0])
        .u32("not_in_schema", vec![0])
        .build()
        .unwrap_err();
    assert!(extra.to_string().contains("extra column"));

    let type_mismatch = AuraColumnBatch::builder(schema)
        .i64("ts_event", vec![1])
        .u64("symbol_id", vec![1])
        .i64("price", vec![10])
        .u64("size", vec![1])
        .u8("side", vec![1])
        .u32("flags", vec![0])
        .build()
        .unwrap_err();
    assert!(type_mismatch.to_string().contains("column type"));
}

#[test]
fn reader_streaming_batches_match_read_batches_and_replay() {
    let schema = market_schema();
    let rows = market_rows();
    let aura1 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura1());
    let mut reader = AuraReader::open(Cursor::new(&aura1)).unwrap();

    assert_eq!(schema.fields(), reader.schema().fields());
    let open_stats = reader.stats();
    assert_eq!(0, open_stats.open_decoded_row_count);
    assert!(!open_stats.full_file_materialized);
    assert!(open_stats.streaming_reader_used);
    let first = reader.next_batch(2).unwrap().unwrap();
    let first_stats = reader.stats();
    assert_eq!(2, first_stats.rows_decoded_in_last_batch);
    assert_eq!(2, first_stats.max_rows_materialized_at_once);
    assert!(!first_stats.full_file_materialized);
    let second = reader.next_batch(2).unwrap().unwrap();
    assert!(reader.next_batch(2).unwrap().is_none());
    assert_eq!(&rows[..2], first.rows());
    assert_eq!(&rows[2..], second.rows());

    reader.reset_batches();
    assert_eq!(3, reader.next_batch(3).unwrap().unwrap().row_count());
    let iter_batches = reader
        .batches(2)
        .unwrap()
        .collect::<aura_codec::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(2, iter_batches.len());

    let mut replayed = Vec::new();
    let count = reader
        .replay_i64(|row| {
            replayed.push(row.to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(rows.len(), count);
    assert_eq!(
        AuraRecordBatch::new(schema, rows.clone())
            .unwrap()
            .to_i64_rows()
            .unwrap(),
        replayed
    );
}

#[test]
fn reader_next_column_batch_matches_row_batches() {
    let schema = market_schema();
    let rows = market_rows();
    let aura1 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura1());
    let mut reader = AuraReader::open(Cursor::new(&aura1)).unwrap();

    let first = reader.next_column_batch(2).unwrap().unwrap();
    assert_eq!(2, first.row_count());
    assert_eq!(
        &rows[..2],
        first.clone().into_record_batch().unwrap().rows()
    );
    let stats = reader.stats();
    assert_eq!(2, stats.rows_decoded_in_last_batch);
    assert_eq!(0, stats.max_rows_materialized_at_once);

    let second = reader.next_column_batch(2).unwrap().unwrap();
    assert_eq!(1, second.row_count());
    assert_eq!(&rows[2..], second.into_record_batch().unwrap().rows());
    assert!(reader.next_column_batch(2).unwrap().is_none());

    let row_reader = AuraReader::open(Cursor::new(&aura1)).unwrap();
    let row_batches = row_reader.read_batches().unwrap();
    let all_rows = first
        .into_record_batch()
        .unwrap()
        .rows()
        .iter()
        .cloned()
        .chain(rows[2..].iter().cloned())
        .collect::<Vec<_>>();
    assert_eq!(row_batches[0].rows(), all_rows.as_slice());
}

#[test]
fn aura1_row_view_and_batch_field_iterators_match_rows() {
    let schema = market_schema();
    let rows = market_rows();
    let expected = AuraRecordBatch::new(schema.clone(), rows.clone())
        .unwrap()
        .to_i64_rows()
        .unwrap();
    let aura1 = write_with_options(schema, rows, WriterOptions::aura1());

    let reader = AuraReader::open(Cursor::new(&aura1)).unwrap();
    let mut replayed = Vec::new();
    let mut checksums = Vec::new();
    let count = reader
        .replay_row_views(|row| {
            let mut values = Vec::with_capacity(row.field_count());
            for field_index in 0..row.field_count() {
                values.push(row.get_i64(field_index)?);
            }
            checksums.push(row.checksum_all_fields()?);
            replayed.push(values);
            Ok(())
        })
        .unwrap();
    assert_eq!(expected.len(), count);
    assert_eq!(expected, replayed);
    assert_eq!(expected.len(), checksums.len());
    let stats = reader.stats();
    assert_eq!(expected.len(), stats.visitor_calls);
    assert_eq!(expected.len(), stats.rows_scanned);

    let batch_reader = AuraReader::open(Cursor::new(&aura1)).unwrap();
    let mut prices = Vec::new();
    let mut batch_checksum = 0u64;
    batch_reader
        .replay_fixed_batches(2, |batch| {
            for value in batch.field_i64(2)? {
                prices.push(value?);
            }
            batch_checksum ^= batch.checksum_all_fields()?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        expected.iter().map(|row| row[2]).collect::<Vec<_>>(),
        prices
    );
    assert_ne!(0, batch_checksum);
    assert_eq!(2, batch_reader.stats().visitor_calls);
}

#[test]
fn aura1_all_field_parse_kernels_match_reordered_mixed_width_schema() {
    let schema = AuraSchema::builder()
        .field("venue_flags", AuraType::FlagsU32)
        .field("px", AuraType::PriceI64Scaled { scale: 4 })
        .field("side_code", AuraType::EnumU8)
        .field("event_time", AuraType::TimestampNanos)
        .field("qty", AuraType::U64)
        .field("symbol_key", AuraType::U32)
        .build()
        .unwrap();
    let rows = vec![
        vec![
            1_u64.into(),
            100_0100_i64.into(),
            1_u64.into(),
            1_700_000_000_000_000_000_i64.into(),
            10_u64.into(),
            42_u64.into(),
        ],
        vec![
            3_u64.into(),
            100_0200_i64.into(),
            2_u64.into(),
            1_700_000_000_000_000_100_i64.into(),
            11_u64.into(),
            42_u64.into(),
        ],
        vec![
            7_u64.into(),
            99_9900_i64.into(),
            1_u64.into(),
            1_700_000_000_000_000_200_i64.into(),
            12_u64.into(),
            77_u64.into(),
        ],
    ];
    let aura1 = write_with_options(schema, rows, WriterOptions::aura1());
    let reader = AuraReader::open(Cursor::new(&aura1)).unwrap();

    let mut field_major = 0u64;
    let mut checked_once = 0u64;
    let mut type_kernel = 0u64;
    let mut parse_program = 0u64;
    let mut selected_all = 0u64;
    reader
        .replay_fixed_batches(2, |batch| {
            let all_fields = (0..batch.field_count()).collect::<Vec<_>>();
            field_major = field_major.wrapping_add(batch.checksum_all_fields_field_major()?);
            checked_once = checked_once.wrapping_add(batch.checksum_all_fields_checked_once()?);
            type_kernel = type_kernel.wrapping_add(batch.checksum_all_fields_type_kernel()?);
            parse_program = parse_program.wrapping_add(batch.checksum_all_fields_parse_program()?);
            selected_all = selected_all.wrapping_add(batch.checksum_selected_fields(&all_fields)?);
            Ok(())
        })
        .unwrap();

    assert_ne!(0, field_major);
    assert_eq!(field_major, checked_once);
    assert_eq!(field_major, type_kernel);
    assert_eq!(field_major, parse_program);
    assert_eq!(field_major, selected_all);
}

#[test]
fn aura1_file_backed_replay_reads_header_footer_at_open() {
    let schema = market_schema();
    let rows = market_rows();
    let aura1 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura1());
    let path = write_temp_file("file_backed_replay", &aura1);

    let mut reader = AuraReader::open_path(&path).unwrap();
    let layout = aura_codec::records::aura1_fixed_layout_info(&aura1).unwrap();
    assert_eq!(schema.fields(), reader.schema().fields());
    let open_stats = reader.stats();
    assert_eq!(AuraReaderSourceKind::FileRange, open_stats.source_kind);
    assert_eq!(AuraReplayBackend::FileRange, open_stats.replay_backend);
    assert_eq!(aura1.len(), open_stats.file_len);
    assert!(open_stats.bytes_read_at_open < aura1.len());
    assert_eq!(0, open_stats.body_bytes_read_at_open);
    assert_eq!(0, open_stats.full_file_bytes_copied);
    assert_eq!(3, open_stats.record_count_from_footer);
    assert_eq!(layout.record_width, open_stats.row_width_from_plan);
    assert_eq!(layout.record_count, open_stats.record_count_from_footer);
    assert!(open_stats.body_offset_from_header < open_stats.footer_offset_from_trailer);
    assert_eq!(
        open_stats.bytes_read_at_open,
        open_stats.source_bytes_read_total
    );

    let first = reader.next_batch(2).unwrap().unwrap();
    assert_eq!(&rows[..2], first.rows());
    let first_stats = reader.stats();
    assert_eq!(2, first_stats.rows_decoded_in_last_batch);
    assert_eq!(2, first_stats.max_rows_materialized_at_once);
    assert_eq!(
        2 * first_stats.row_width_from_plan,
        first_stats.bytes_read_in_last_batch
    );
    assert_eq!(
        first_stats.bytes_read_at_open + first_stats.bytes_read_in_last_batch,
        first_stats.source_bytes_read_total
    );

    let second = reader.next_column_batch(2).unwrap().unwrap();
    assert_eq!(1, second.row_count());
    assert_eq!(&rows[2..], second.into_record_batch().unwrap().rows());
    let second_stats = reader.stats();
    assert_eq!(
        second_stats.row_width_from_plan,
        second_stats.bytes_read_in_last_batch
    );
    assert!(!second_stats.full_file_materialized);

    let replay_reader = AuraReader::open_file(File::open(&path).unwrap()).unwrap();
    let mut replayed = Vec::new();
    let count = replay_reader
        .replay_i64(|row| {
            replayed.push(row.to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(rows.len(), count);
    assert_eq!(
        AuraRecordBatch::new(schema, rows.clone())
            .unwrap()
            .to_i64_rows()
            .unwrap(),
        replayed
    );
    let replay_stats = replay_reader.stats();
    assert_eq!(
        replay_stats.record_count_from_footer * replay_stats.row_width_from_plan,
        replay_stats.bytes_read_during_replay
    );
    assert_eq!(0, replay_stats.full_file_bytes_copied);

    let batch_reader = AuraReader::open_path(&path).unwrap();
    let mut batch_rows = 0usize;
    let mut first_values = Vec::new();
    let batch_count = batch_reader
        .replay_fixed_batches(2, |batch| {
            batch_rows = batch_rows.saturating_add(batch.row_count());
            if batch.row_count() > 0 {
                first_values.push(batch.value_i64(0, 0)?);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(rows.len(), batch_count);
    assert_eq!(rows.len(), batch_rows);
    assert_eq!(
        vec![1_700_000_000_000_000_000, 1_700_000_000_002_000_000],
        first_values
    );
    let batch_stats = batch_reader.stats();
    assert_eq!(2, batch_stats.visitor_calls);
    assert_eq!(0, batch_stats.full_file_bytes_copied);
}

#[test]
fn aura1_file_backed_open_rejects_bad_seal_and_truncated_body() {
    let schema = market_schema();
    let rows = market_rows();
    let aura1 = write_with_options(schema, rows, WriterOptions::aura1());

    let mut bad_seal = aura1.clone();
    let last = bad_seal.last_mut().unwrap();
    *last ^= 0x01;
    let bad_seal_path = write_temp_file("bad_seal", &bad_seal);
    assert!(AuraReader::open_path(&bad_seal_path).is_err());

    let info = aura_codec::records::aura1_fixed_layout_info(&aura1).unwrap();
    let mut truncated_body = Vec::new();
    truncated_body.extend_from_slice(&aura1[..info.footer_offset - 1]);
    truncated_body.extend_from_slice(&aura1[info.footer_offset..]);
    let truncated_path = write_temp_file("truncated_body", &truncated_body);
    assert!(AuraReader::open_path(&truncated_path).is_err());
}

#[test]
fn grouped_replay_uses_generic_field_names_and_preserves_runs() {
    let schema = AuraSchema::named("grouped_non_grimoire")
        .field("venue", AuraType::U16)
        .field("px", AuraType::I64Scaled { scale: 4 })
        .field("event_time", AuraType::TimestampNanos)
        .field("symbol_code", AuraType::U32)
        .build()
        .unwrap();
    let rows = vec![
        vec![1_u64.into(), 100_i64.into(), 1_000_i64.into(), 7_u64.into()],
        vec![1_u64.into(), 101_i64.into(), 1_000_i64.into(), 7_u64.into()],
        vec![1_u64.into(), 102_i64.into(), 2_000_i64.into(), 8_u64.into()],
        vec![2_u64.into(), 103_i64.into(), 3_000_i64.into(), 8_u64.into()],
        vec![2_u64.into(), 104_i64.into(), 3_000_i64.into(), 8_u64.into()],
        vec![2_u64.into(), 105_i64.into(), 3_000_i64.into(), 9_u64.into()],
    ];
    let aura1 = write_with_options(schema.clone(), rows, WriterOptions::aura1());
    let reader = AuraReader::open(Cursor::new(&aura1)).unwrap();

    let mut groups = Vec::new();
    let stats = reader
        .grouped_replay(&GroupBy::fields(["event_time"]), |group| {
            groups.push((
                group.row_start(),
                group.row_count(),
                group.key().field_names().to_vec(),
                group.key().values().to_vec(),
            ));
            Ok(())
        })
        .unwrap();
    assert_eq!(6, stats.row_count);
    assert_eq!(3, stats.group_count);
    assert_eq!(3, stats.rows_per_group_p95);
    assert_eq!(
        vec![2, 1, 3],
        groups.iter().map(|group| group.1).collect::<Vec<_>>()
    );
    assert_eq!(vec!["event_time".to_string()], groups[0].2);
    assert_eq!(vec![AuraValue::I64(1_000)], groups[0].3);
    assert_eq!(0, groups[0].0);
    assert_eq!(3, groups[2].0);

    let mut pair_counts = Vec::new();
    reader
        .grouped_replay(&GroupBy::fields(["event_time", "symbol_code"]), |group| {
            pair_counts.push(group.row_count());
            Ok(())
        })
        .unwrap();
    assert_eq!(vec![2, 1, 2, 1], pair_counts);

    let path = write_temp_file("grouped_file_backed", &aura1);
    let file_reader = AuraReader::open_path(&path).unwrap();
    let mut file_pair_counts = Vec::new();
    file_reader
        .grouped_replay(&GroupBy::fields(["event_time", "symbol_code"]), |group| {
            file_pair_counts.push(group.row_count());
            Ok(())
        })
        .unwrap();
    assert_eq!(pair_counts, file_pair_counts);
    let file_stats = file_reader.stats();
    assert_eq!(AuraReaderSourceKind::FileRange, file_stats.source_kind);
    assert_eq!(0, file_stats.body_bytes_read_at_open);
    assert_eq!(0, file_stats.full_file_bytes_copied);
    assert_eq!(file_pair_counts.len(), file_stats.visitor_calls);
    assert_eq!(file_pair_counts.len() * 2, file_stats.field_decode_count);
    assert_eq!(6, file_stats.rows_scanned);

    let mut id_counts = Vec::new();
    reader
        .grouped_replay(&GroupBy::field_ids([2]), |group| {
            id_counts.push(group.row_count());
            Ok(())
        })
        .unwrap();
    assert_eq!(vec![2, 1, 3], id_counts);

    let mut high_cardinality = 0usize;
    let stats = reader
        .grouped_replay(&GroupBy::fields(["px"]), |group| {
            assert_eq!(1, group.row_count());
            high_cardinality = high_cardinality.saturating_add(1);
            Ok(())
        })
        .unwrap();
    assert_eq!(6, high_cardinality);
    assert_eq!(6, stats.group_count);

    let error = reader
        .grouped_replay(&GroupBy::fields(["missing_field"]), |_| Ok(()))
        .unwrap_err();
    assert!(error.to_string().contains("group field"));
}

#[test]
fn convert_aura0_to_aura1_generic_schema_preserves_rows() {
    let schema = market_schema();
    let rows = market_rows();
    let aura0 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura0_compact());
    let mut aura1 = Vec::new();

    let summary = convert_aura(
        Cursor::new(&aura0),
        &mut aura1,
        ConvertOptions::new(AuraFormat::Aura1).verify(true),
    )
    .unwrap();

    assert!(summary.verified);
    assert_eq!(AuraFormat::Aura0, summary.source_format);
    assert_eq!(AuraFormat::Aura1, summary.target_format);
    assert_roundtrip(&aura1, &schema, &rows);
}

#[test]
fn convert_aura1_to_aura0_generic_schema_preserves_rows() {
    let schema = market_schema();
    let rows = market_rows();
    let aura1 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura1());
    let mut aura0 = Vec::new();

    let summary = convert_aura(
        Cursor::new(&aura1),
        &mut aura0,
        ConvertOptions::new(AuraFormat::Aura0).verify(true),
    )
    .unwrap();

    assert!(summary.verified);
    assert_eq!(AuraFormat::Aura1, summary.source_format);
    assert_eq!(AuraFormat::Aura0, summary.target_format);
    assert_roundtrip(&aura0, &schema, &rows);
}

#[test]
fn generated_reordered_wide_and_narrow_schemas_roundtrip() {
    let cases = vec![
        (
            AuraSchema::named("narrow")
                .field("event_time", AuraType::TimestampMicros)
                .field("quantity", AuraType::I32)
                .build()
                .unwrap(),
            vec![
                vec![123_000_i64.into(), 10_i64.into()],
                vec![124_000_i64.into(), (-3_i64).into()],
            ],
        ),
        (
            AuraSchema::named("reordered")
                .field("flags", AuraType::FlagsU32)
                .field("side", AuraType::EnumU8)
                .field("px", AuraType::I64Scaled { scale: 4 })
                .field("ts", AuraType::TimestampNanos)
                .build()
                .unwrap(),
            vec![
                vec![1_u64.into(), 2_u64.into(), 105_000_i64.into(), 9_i64.into()],
                vec![
                    0_u64.into(),
                    1_u64.into(),
                    104_999_i64.into(),
                    10_i64.into(),
                ],
            ],
        ),
        (
            AuraSchema::named("wide_no_grimoire_names")
                .field("alpha_ts", AuraType::TimestampNanos)
                .field("beta_id", AuraType::U16)
                .field("gamma_px", AuraType::PriceI64Scaled { scale: 2 })
                .field("delta_qty", AuraType::U32)
                .field("epsilon_bool", AuraType::Bool)
                .field("zeta_flags", AuraType::FlagsU32)
                .field("eta_change", AuraType::I16)
                .field("theta_code", AuraType::EnumU8)
                .field("iota_count", AuraType::U8)
                .build()
                .unwrap(),
            vec![
                vec![
                    1_000_i64.into(),
                    7_u64.into(),
                    123_45_i64.into(),
                    50_u64.into(),
                    true.into(),
                    3_u64.into(),
                    (-2_i64).into(),
                    4_u64.into(),
                    9_u64.into(),
                ],
                vec![
                    2_000_i64.into(),
                    8_u64.into(),
                    123_50_i64.into(),
                    51_u64.into(),
                    false.into(),
                    2_u64.into(),
                    2_i64.into(),
                    5_u64.into(),
                    10_u64.into(),
                ],
            ],
        ),
    ];

    for (schema, rows) in cases {
        let aura1 = write_with_options(schema.clone(), rows.clone(), WriterOptions::aura1());
        let aura0 =
            write_with_options(schema.clone(), rows.clone(), WriterOptions::aura0_compact());

        assert_roundtrip(&aura1, &schema, &rows);
        assert_roundtrip(&aura0, &schema, &rows);
    }
}

#[test]
fn writer_rejects_out_of_range_values_for_declared_types() {
    let schema = AuraSchema::builder()
        .field("side", AuraType::EnumU8)
        .field("signed", AuraType::I8)
        .build()
        .unwrap();
    let rows = vec![vec![300_u64.into(), 0_i64.into()]];
    let mut bytes = Vec::new();
    let mut writer =
        AuraWriter::try_new(&mut bytes, schema.clone(), WriterOptions::aura1()).unwrap();
    let err = writer
        .write_batch(AuraRecordBatch::new(schema, rows).unwrap())
        .unwrap_err();

    assert!(err.to_string().contains("unsigned value"));
}
