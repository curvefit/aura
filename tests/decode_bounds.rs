use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::{fs, path::PathBuf};

use aura_codec::records::{
    self, Aura0DecodePath, Aura0EncoderPath, I64FileInput, OutputGuardMode, TranscodePath,
};
use aura_codec::schema::{generic_i64_parent_schema, ohlcv_schema};
use aura_codec::writer;
use aura_codec::{
    decode_generic_i64_rows, decode_generic_stream_body, Aura0ColumnPath, Aura1BodyPath,
    Aura1ExecutionOptions, AuraError, AuraHeader, AuraI64Reader, AuraReader, AuraTypedValue,
    AuraTypedWriter, FieldRole, FieldType, GenericEncodedI64Rows, GenericInstructionPlan,
    GenericStreamInstruction, GenericStreamOp, Profile, SchemaBuilder, UnsupportedPathBehavior,
};

struct Profiles {
    ingest: Vec<u8>,
    compact: Vec<u8>,
    hybrid: Vec<u8>,
    fast: Vec<u8>,
    aura1: Vec<u8>,
}

fn sample_profiles() -> Profiles {
    let ingest = writer::encode_i64(I64FileInput {
        schema: ohlcv_schema().unwrap(),
        rows: vec![
            vec![1_000, 10, 11, 9, 10, 100],
            vec![2_000, 11, 12, 10, 11, 101],
        ],
        stream_id: 1,
        dictionary_id: 2,
        header_comment: Some("decode-bounds".to_owned()),
    })
    .unwrap();
    let aura1 = writer::compile_i64(&ingest, Profile::Aura1).unwrap();
    let compact = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let hybrid = records::compile_i64_file_with_aura0_profile(
        &aura1,
        records::Aura0FileProfile::Hybrid,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    let fast = records::compile_i64_file_with_aura0_profile(
        &aura1,
        records::Aura0FileProfile::Fast,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    Profiles {
        ingest,
        compact,
        hybrid,
        fast,
        aura1,
    }
}

fn footer_start(bytes: &[u8]) -> usize {
    let seal_offset = bytes.len() - b"sealed:)".len();
    let footer_len_offset = seal_offset - 4;
    let footer_len =
        u32::from_le_bytes(bytes[footer_len_offset..seal_offset].try_into().unwrap()) as usize;
    footer_len_offset - footer_len
}

fn with_compiled_record_count(bytes: &[u8], record_count: u64) -> Vec<u8> {
    let mut mutated = bytes.to_vec();
    let count_offset = footer_start(&mutated) + 8;
    mutated[count_offset..count_offset + 8].copy_from_slice(&record_count.to_le_bytes());
    mutated
}

fn truncate_one_body_byte(bytes: &[u8]) -> Vec<u8> {
    let mut mutated = bytes.to_vec();
    let header_len = AuraHeader::encoded_len(&mutated).unwrap();
    assert!(header_len < footer_start(&mutated));
    mutated.remove(header_len);
    mutated
}

fn assert_bounded_error<T: std::fmt::Debug>(result: aura_codec::Result<T>) {
    let error = result.expect_err("hostile input unexpectedly decoded");
    assert!(matches!(
        error,
        AuraError::InvalidValue(_)
            | AuraError::UnexpectedEof
            | AuraError::InvalidMagic { .. }
            | AuraError::TrailingBytes(_)
    ));
}

#[test]
fn compiled_u64_max_counts_fail_before_row_allocation_for_every_profile() {
    let Profiles {
        compact,
        hybrid,
        fast,
        aura1,
        ..
    } = sample_profiles();
    for (source, is_aura0) in [
        (&compact, true),
        (&hybrid, true),
        (&fast, true),
        (&aura1, false),
    ] {
        let hostile = with_compiled_record_count(source, u64::MAX);
        let decoded = catch_unwind(AssertUnwindSafe(|| records::decode_i64_file(&hostile)));
        assert!(decoded.is_ok(), "decoder panicked on u64::MAX count");
        assert!(matches!(
            decoded.unwrap().unwrap_err(),
            AuraError::InvalidValue("i64 decode row limit")
                | AuraError::InvalidValue("i64 decode record count")
        ));
        if is_aura0 {
            let materialized = catch_unwind(AssertUnwindSafe(|| {
                records::compile_aura0_to_aura1_materialized(&hostile)
            }));
            assert!(materialized.is_ok(), "materialized compiler panicked");
            assert!(matches!(
                materialized.unwrap().unwrap_err(),
                AuraError::InvalidValue("i64 decode row limit")
                    | AuraError::InvalidValue("i64 decode record count")
            ));
        }
    }
}

#[test]
fn row_field_product_limit_rejects_before_matrix_allocation() {
    let mut parents = vec![0u8; 64];
    parents[0] = 100;
    let ingest = writer::encode_i64(I64FileInput {
        schema: generic_i64_parent_schema("decode-value-limit", &parents).unwrap(),
        rows: vec![vec![0; 64]],
        stream_id: 0,
        dictionary_id: 0,
        header_comment: None,
    })
    .unwrap();
    let compact = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let hostile_count = u64::try_from(records::MAX_V2_I64_DECODE_VALUES / 64 + 1).unwrap();
    assert!(usize::try_from(hostile_count).unwrap() <= records::MAX_V2_I64_DECODE_ROWS);
    let hostile = with_compiled_record_count(&compact, hostile_count);
    let result = catch_unwind(AssertUnwindSafe(|| records::decode_i64_file(&hostile)));
    assert!(result.is_ok());
    assert!(matches!(
        result.unwrap().unwrap_err(),
        AuraError::InvalidValue("i64 decode value limit")
    ));
}

#[test]
fn raw_body_counts_and_fields_are_checked_before_allocation() {
    let ingest = sample_profiles().ingest;
    let header_len = AuraHeader::encoded_len(&ingest).unwrap();

    let mut huge_rows = ingest.clone();
    huge_rows[header_len..header_len + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    let result = catch_unwind(AssertUnwindSafe(|| records::decode_i64_file(&huge_rows)));
    assert!(result.is_ok());
    assert!(matches!(
        result.unwrap().unwrap_err(),
        AuraError::InvalidValue("i64 decode row limit")
            | AuraError::InvalidValue("i64 decode record count")
    ));

    let mut huge_fields = ingest;
    huge_fields[header_len + 8..header_len + 10].copy_from_slice(&u16::MAX.to_le_bytes());
    let result = catch_unwind(AssertUnwindSafe(|| records::decode_i64_file(&huge_fields)));
    assert!(result.is_ok());
    assert!(matches!(
        result.unwrap().unwrap_err(),
        AuraError::InvalidValue("i64 decode field limit")
    ));
}

#[test]
fn truncated_bodies_fail_without_panics_for_all_storage_shapes() {
    let Profiles {
        ingest,
        compact,
        hybrid,
        fast,
        aura1,
    } = sample_profiles();
    for source in [&ingest, &compact, &hybrid, &fast, &aura1] {
        let hostile = truncate_one_body_byte(source);
        let result = catch_unwind(AssertUnwindSafe(|| records::decode_i64_file(&hostile)));
        assert!(result.is_ok(), "decoder panicked on truncated body");
        assert_bounded_error(result.unwrap());
    }
}

#[test]
fn public_generic_decoder_rejects_usize_dimension_bombs() {
    let hostile = GenericEncodedI64Rows {
        plan: GenericInstructionPlan {
            streams: Vec::new(),
            groups: Vec::new(),
        },
        streams: Vec::new(),
        record_count: usize::MAX,
        field_count: records::MAX_V2_I64_DECODE_FIELDS,
    };
    let result = catch_unwind(AssertUnwindSafe(|| decode_generic_i64_rows(&hostile)));
    assert!(result.is_ok());
    assert!(matches!(
        result.unwrap().unwrap_err(),
        AuraError::InvalidValue("i64 decode row limit")
            | AuraError::InvalidValue("i64 decode record count")
            | AuraError::InvalidValue("i64 decode value count")
    ));
}

#[test]
fn hostile_compiled_counts_reject_public_reader_and_compile_bypasses() {
    let hostile = with_compiled_record_count(&sample_profiles().compact, u64::MAX);
    let columns = catch_unwind(AssertUnwindSafe(|| {
        records::decode_i64_columns_file(&hostile)
    }));
    assert!(columns.is_ok());
    assert_bounded_error(columns.unwrap());

    let legacy = catch_unwind(AssertUnwindSafe(|| {
        records::try_compile_i64_file_profiled(
            &hostile,
            Profile::Aura1,
            OutputGuardMode::NoGuard,
            TranscodePath::Auto,
            Aura0EncoderPath::Materialized,
            Aura0DecodePath::Materialized,
        )
    }));
    assert!(legacy.is_ok());
    assert_bounded_error(legacy.unwrap());

    for body_path in [
        Aura1BodyPath::Columns,
        Aura1BodyPath::DirectFile,
        Aura1BodyPath::DirectStreams,
        Aura1BodyPath::StreamingCursor,
    ] {
        let typed = catch_unwind(AssertUnwindSafe(|| {
            records::try_compile_i64_file_profiled_with_options(
                &hostile,
                Profile::Aura1,
                OutputGuardMode::NoGuard,
                TranscodePath::Auto,
                Aura1ExecutionOptions {
                    body_path,
                    column_path: Aura0ColumnPath::Materialized,
                    unsupported_path: UnsupportedPathBehavior::Error,
                },
            )
        }));
        assert!(typed.is_ok(), "{body_path:?} panicked");
        assert_bounded_error(typed.unwrap());
    }

    for result in [
        catch_unwind(AssertUnwindSafe(|| {
            records::decode_i64_file_metadata(&hostile).map(|_| ())
        })),
        catch_unwind(AssertUnwindSafe(|| {
            AuraI64Reader::open(&hostile).map(|_| ())
        })),
        catch_unwind(AssertUnwindSafe(|| {
            AuraReader::open(Cursor::new(hostile.clone())).map(|_| ())
        })),
    ] {
        assert!(result.is_ok());
        assert_bounded_error(result.unwrap());
    }

    let hostile_aura1 = with_compiled_record_count(&sample_profiles().aura1, u64::MAX);
    let memory_reader = catch_unwind(AssertUnwindSafe(|| {
        AuraReader::open(Cursor::new(hostile_aura1.clone())).map(|_| ())
    }));
    assert!(memory_reader.is_ok());
    assert_bounded_error(memory_reader.unwrap());

    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "decode-bounds-hostile-reader-{}.aura1",
        std::process::id()
    ));
    fs::write(&path, hostile_aura1).unwrap();
    let file_reader = catch_unwind(AssertUnwindSafe(|| {
        AuraReader::open_path(&path).map(|_| ())
    }));
    assert!(file_reader.is_ok());
    assert_bounded_error(file_reader.unwrap());
    fs::remove_file(path).unwrap();
}

#[test]
fn hostile_huffman_dictionary_and_rle_counts_reject_before_allocation() {
    let empty = GenericInstructionPlan {
        streams: Vec::new(),
        groups: Vec::new(),
    }
    .encode()
    .unwrap();
    let mut legacy_huffman = Vec::new();
    legacy_huffman.extend_from_slice(&empty[..5]);
    legacy_huffman.extend_from_slice(&1u16.to_le_bytes());
    legacy_huffman.extend_from_slice(&0u16.to_le_bytes());
    legacy_huffman.extend_from_slice(&0u16.to_le_bytes());
    legacy_huffman.extend_from_slice(&u16::MAX.to_le_bytes());
    legacy_huffman.push(11);
    legacy_huffman.extend_from_slice(&0i64.to_le_bytes());
    legacy_huffman.extend_from_slice(&1i64.to_le_bytes());
    legacy_huffman.extend_from_slice(&u32::MAX.to_le_bytes());
    legacy_huffman.push(1);
    let result = catch_unwind(AssertUnwindSafe(|| {
        GenericInstructionPlan::decode(&legacy_huffman)
    }));
    assert!(result.is_ok());
    assert!(matches!(
        result.unwrap().unwrap_err(),
        AuraError::InvalidValue("dictionary entry count")
    ));

    for op in [
        GenericStreamOp::Dictionary {
            unit: 1,
            entry_count: u32::MAX,
            code_width: 1,
        },
        GenericStreamOp::PackedDictionary {
            base: 0,
            unit: 1,
            entry_count: u32::MAX,
            entry_width: 1,
            code_width: 1,
        },
        GenericStreamOp::Rle {
            base: 0,
            unit: 1,
            bit_width: 1,
            run_count: u32::MAX,
        },
    ] {
        let instruction = GenericStreamInstruction {
            stream_id: 0,
            target_slot: Some(0),
            op,
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            decode_generic_stream_body(&instruction, &[], 1)
        }));
        assert!(result.is_ok());
        assert_bounded_error(result.unwrap());
    }
}

#[test]
fn zero_width_rows_reject_below_global_row_limit() {
    let hostile = GenericEncodedI64Rows {
        plan: GenericInstructionPlan {
            streams: Vec::new(),
            groups: Vec::new(),
        },
        streams: Vec::new(),
        record_count: 1,
        field_count: 0,
    };
    let result = catch_unwind(AssertUnwindSafe(|| decode_generic_i64_rows(&hostile)));
    assert!(result.is_ok());
    assert!(matches!(
        result.unwrap().unwrap_err(),
        AuraError::InvalidValue("zero-width record count")
    ));
}

#[test]
fn typed_uuid_stream_count_bomb_rejects_before_u128_reserve() {
    let schema = SchemaBuilder::new("uuid-count-bomb")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("id", FieldType::Opaque16, FieldRole::Identifier)
        .finish()
        .unwrap();
    let mut writer = AuraTypedWriter::new(schema);
    writer
        .push_row(vec![
            AuraTypedValue::I64(1),
            AuraTypedValue::Opaque16([7; 16]),
        ])
        .unwrap();
    let ingest = writer.finish().unwrap();
    let mut aura0 = records::compile_typed_file(&ingest, Profile::Aura0).unwrap();
    let body_start = AuraHeader::encoded_len(&aura0).unwrap();
    let body_end = footer_start(&aura0);
    let stream_count = u16::from_le_bytes(aura0[body_start..body_start + 2].try_into().unwrap());
    assert!(stream_count >= 2);
    let mut cursor = body_start + 2;
    let mut last_value_count_offset = None;
    for _ in 0..stream_count {
        let value_count_offset = cursor + 2;
        let body_len_offset = cursor + 10;
        let stream_body_len = u32::from_le_bytes(
            aura0[body_len_offset..body_len_offset + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        last_value_count_offset = Some(value_count_offset);
        cursor = body_len_offset + 4 + stream_body_len;
    }
    assert_eq!(body_end, cursor);
    let offset = last_value_count_offset.unwrap();
    aura0[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    let result = catch_unwind(AssertUnwindSafe(|| records::decode_typed_file(&aura0)));
    assert!(result.is_ok());
    let error = result.unwrap().unwrap_err();
    assert!(
        matches!(
            error,
            AuraError::InvalidValue("generic stream value limit")
                | AuraError::InvalidValue("generic stream value count")
                | AuraError::InvalidValue("stream value count")
        ),
        "unexpected error: {error:?}"
    );
}
