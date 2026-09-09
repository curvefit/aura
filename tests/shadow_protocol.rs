use std::io::Cursor;
use std::io::{self, Read};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, FixedSizeBinaryArray, Int16Array, Int32Array, Int64Array, Int8Array, StringArray,
    TimestampMillisecondArray, TimestampNanosecondArray, UInt16Array, UInt32Array, UInt64Array,
    UInt8Array,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::ipc::root_as_message;
use arrow::ipc::writer::{IpcWriteOptions, StreamWriter};
use arrow::ipc::MetadataVersion;
use arrow::record_batch::RecordBatch;
use aura_codec::experimental::{
    decode_v3_value_block, encode_shadow_arrow_ipc, ShadowProtocolLimits, V3ValueLimits,
};
use aura_codec::{AuraV3ValueRef, FieldRole, FieldType, SchemaBuilder, SchemaDescriptor};

fn aura_schema() -> SchemaDescriptor {
    SchemaBuilder::new("shadow_test")
        .v3()
        .nullable_field("i8", FieldType::I8, FieldRole::Value)
        .nullable_field("u8_bool", FieldType::U8, FieldRole::Boolean)
        .nullable_field("i16", FieldType::I16, FieldRole::Value)
        .nullable_field("u16", FieldType::U16, FieldRole::Value)
        .nullable_field("i32", FieldType::I32, FieldRole::Value)
        .nullable_field("u32", FieldType::U32, FieldRole::Value)
        .nullable_field("i64", FieldType::I64, FieldRole::Value)
        .nullable_field("u64", FieldType::U64, FieldRole::Value)
        .nullable_field("ns", FieldType::TimestampNs, FieldRole::Value)
        .nullable_field("i128", FieldType::I128, FieldRole::Value)
        .nullable_field("opaque", FieldType::Opaque16, FieldRole::Identifier)
        .nullable_field("text", FieldType::Utf8, FieldRole::Value)
        .nullable_field("decimal", FieldType::DecimalText, FieldRole::Value)
        .finish()
        .unwrap()
}

fn arrow_schema(schema: &SchemaDescriptor) -> Arc<Schema> {
    Arc::new(Schema::new(
        schema
            .fields
            .iter()
            .map(|field| {
                let data_type = match field.field_type {
                    FieldType::I8 => DataType::Int8,
                    FieldType::U8 => DataType::UInt8,
                    FieldType::I16 => DataType::Int16,
                    FieldType::U16 => DataType::UInt16,
                    FieldType::I32 => DataType::Int32,
                    FieldType::U32 => DataType::UInt32,
                    FieldType::I64 => DataType::Int64,
                    FieldType::U64 => DataType::UInt64,
                    FieldType::TimestampNs => DataType::Timestamp(TimeUnit::Nanosecond, None),
                    FieldType::TimestampMs => DataType::Timestamp(TimeUnit::Millisecond, None),
                    FieldType::I128 | FieldType::Opaque16 => DataType::FixedSizeBinary(16),
                    FieldType::Utf8 | FieldType::DecimalText => DataType::Utf8,
                };
                Field::new(&field.name, data_type, field.nullable)
            })
            .collect::<Vec<_>>(),
    ))
}

fn batch(schema: Arc<Schema>, second: bool) -> RecordBatch {
    let (a, b) = if second { (3, 4) } else { (1, 2) };
    let i128_a = (i128::MIN + i128::from(a)).to_le_bytes();
    let i128_b = (i128::MAX - i128::from(b)).to_le_bytes();
    let opaque_a = [a as u8; 16];
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int8Array::from(vec![Some(-(a as i8)), None])),
        Arc::new(UInt8Array::from(vec![
            Some((a % 2) as u8),
            Some((b % 2) as u8),
        ])),
        Arc::new(Int16Array::from(vec![Some(-(a as i16)), Some(b as i16)])),
        Arc::new(UInt16Array::from(vec![Some(a as u16), None])),
        Arc::new(Int32Array::from(vec![Some(-a), Some(b)])),
        Arc::new(UInt32Array::from(vec![Some(a as u32), Some(b as u32)])),
        Arc::new(Int64Array::from(vec![Some(i64::MIN + i64::from(a)), None])),
        Arc::new(UInt64Array::from(vec![Some(u64::MAX - a as u64), Some(0)])),
        Arc::new(TimestampNanosecondArray::from(vec![
            Some(a as i64),
            Some(b as i64),
        ])),
        Arc::new(
            FixedSizeBinaryArray::try_from_iter([i128_a.as_slice(), i128_b.as_slice()].into_iter())
                .unwrap(),
        ),
        Arc::new(
            FixedSizeBinaryArray::try_from_sparse_iter_with_size(
                [Some(opaque_a.as_slice()), None].into_iter(),
                16,
            )
            .unwrap(),
        ),
        Arc::new(StringArray::from(vec![
            Some(""),
            Some(if second { "later" } else { "first" }),
        ])),
        Arc::new(StringArray::from(vec![Some("-0.00"), Some("  +12.340  ")])),
    ];
    RecordBatch::try_new(schema, columns).unwrap()
}

fn stream(schema: Arc<Schema>, batches: &[RecordBatch]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
    for batch in batches {
        writer.write(batch).unwrap();
    }
    writer.finish().unwrap();
    drop(writer);
    bytes
}

fn stream_with_options(
    schema: Arc<Schema>,
    batches: &[RecordBatch],
    options: IpcWriteOptions,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new_with_options(&mut bytes, &schema, options).unwrap();
    for batch in batches {
        writer.write(batch).unwrap();
    }
    writer.finish().unwrap();
    drop(writer);
    bytes
}

#[test]
fn multi_batch_all_types_preserve_source_order_and_exact_values() {
    let schema = aura_schema();
    let arrows = arrow_schema(&schema);
    let bytes = stream(
        arrows.clone(),
        &[batch(arrows.clone(), false), batch(arrows, true)],
    );
    let result = encode_shadow_arrow_ipc(&schema, Cursor::new(bytes), Default::default()).unwrap();
    assert_eq!(result.row_count, 4);
    let decoded = decode_v3_value_block(&schema, &result.block, Default::default()).unwrap();
    assert_eq!(
        decoded.columns[0].validity.as_deref(),
        Some(&[0b0000_0101][..])
    );
    assert_eq!(
        decoded.columns[0].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::I8(-1))
    );
    assert_eq!(
        decoded.columns[1].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::U8(1))
    );
    assert_eq!(
        decoded.columns[2].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::I16(-1))
    );
    assert_eq!(
        decoded.columns[3].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::U16(1))
    );
    assert_eq!(
        decoded.columns[4].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::I32(-1))
    );
    assert_eq!(
        decoded.columns[5].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::U32(1))
    );
    assert_eq!(
        decoded.columns[6].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::I64(i64::MIN + 1))
    );
    assert_eq!(
        decoded.columns[7].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::U64(u64::MAX - 1))
    );
    assert_eq!(
        decoded.columns[7].value_ref(1).unwrap(),
        Some(AuraV3ValueRef::U64(0))
    );
    assert_eq!(
        decoded.columns[8].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::TimestampNs(1))
    );
    assert_eq!(
        decoded.columns[9].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::I128(i128::MIN + 1))
    );
    assert!(matches!(
        decoded.columns[10].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::Opaque16(value)) if value == &[1; 16]
    ));
    assert_eq!(
        decoded.columns[11].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::Utf8(""))
    );
    assert_eq!(
        decoded.columns[11].value_ref(3).unwrap(),
        Some(AuraV3ValueRef::Utf8("later"))
    );
    assert_eq!(
        decoded.columns[12].value_ref(0).unwrap(),
        Some(AuraV3ValueRef::DecimalText("-0.00"))
    );

    let again = encode_shadow_arrow_ipc(
        &schema,
        Cursor::new(stream(
            arrow_schema(&schema),
            &[
                batch(arrow_schema(&schema), false),
                batch(arrow_schema(&schema), true),
            ],
        )),
        Default::default(),
    )
    .unwrap();
    assert_eq!(result, again);
}

#[test]
fn exact_schema_and_unsupported_arrow_types_are_rejected() {
    let schema = aura_schema();
    let good = arrow_schema(&schema);
    let cases = [
        Arc::new(Schema::new(
            good.fields().iter().skip(1).cloned().collect::<Vec<_>>(),
        )),
        Arc::new(Schema::new(
            good.fields()
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    if index == 0 {
                        Field::new(field.name(), field.data_type().clone(), false)
                    } else {
                        field.as_ref().clone()
                    }
                })
                .collect::<Vec<_>>(),
        )),
        Arc::new(Schema::new(
            good.fields()
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    if index == 11 {
                        Field::new(field.name(), DataType::LargeUtf8, true)
                    } else {
                        field.as_ref().clone()
                    }
                })
                .collect::<Vec<_>>(),
        )),
        Arc::new(Schema::new(
            good.fields()
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    if index == 0 {
                        Field::new(field.name(), DataType::Float64, true)
                    } else {
                        field.as_ref().clone()
                    }
                })
                .collect::<Vec<_>>(),
        )),
        Arc::new(Schema::new(
            good.fields()
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    if index == 1 {
                        Field::new(
                            field.name(),
                            DataType::Dictionary(
                                Box::new(DataType::UInt8),
                                Box::new(DataType::UInt8),
                            ),
                            true,
                        )
                    } else {
                        field.as_ref().clone()
                    }
                })
                .collect::<Vec<_>>(),
        )),
        Arc::new(Schema::new(
            good.fields()
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    if index == 0 {
                        Field::new("wrong", field.data_type().clone(), field.is_nullable())
                    } else {
                        field.as_ref().clone()
                    }
                })
                .collect::<Vec<_>>(),
        )),
        Arc::new(Schema::new(
            good.fields()
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    if index == 0 {
                        Field::new(field.name(), DataType::Boolean, field.is_nullable())
                    } else {
                        field.as_ref().clone()
                    }
                })
                .collect::<Vec<_>>(),
        )),
        Arc::new(Schema::new(
            good.fields()
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    if index == 8 {
                        Field::new(
                            field.name(),
                            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                            true,
                        )
                    } else {
                        field.as_ref().clone()
                    }
                })
                .collect::<Vec<_>>(),
        )),
    ];
    for arrow in cases {
        let bytes = stream(arrow, &[]);
        assert!(encode_shadow_arrow_ipc(&schema, Cursor::new(bytes), Default::default()).is_err());
    }

    let mut metadata = std::collections::HashMap::new();
    metadata.insert("secret".to_owned(), "value".to_owned());
    let with_metadata = Arc::new(good.as_ref().clone().with_metadata(metadata));
    assert!(encode_shadow_arrow_ipc(
        &schema,
        Cursor::new(stream(with_metadata, &[])),
        Default::default()
    )
    .is_err());

    let fields = good
        .fields()
        .iter()
        .enumerate()
        .map(|(index, field)| {
            if index == 0 {
                let mut metadata = std::collections::HashMap::new();
                metadata.insert("field".to_owned(), "metadata".to_owned());
                field.as_ref().clone().with_metadata(metadata)
            } else {
                field.as_ref().clone()
            }
        })
        .collect::<Vec<_>>();
    assert!(encode_shadow_arrow_ipc(
        &schema,
        Cursor::new(stream(Arc::new(Schema::new(fields)), &[])),
        Default::default()
    )
    .is_err());
}

struct OneByteReader(Cursor<Vec<u8>>);

impl Read for OneByteReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let length = output.len().min(1);
        self.0.read(&mut output[..length])
    }
}

#[test]
fn bounded_reader_handles_many_small_reads() {
    let schema = aura_schema();
    let arrows = arrow_schema(&schema);
    let bytes = stream(arrows.clone(), &[batch(arrows, false)]);
    let result = encode_shadow_arrow_ipc(
        &schema,
        OneByteReader(Cursor::new(bytes)),
        Default::default(),
    )
    .unwrap();
    assert_eq!(result.row_count, 2);
}

#[test]
fn invalid_aura_schema_is_rejected_before_input_is_read() {
    struct PanicReader;
    impl Read for PanicReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("input must not be consumed")
        }
    }
    let schema = SchemaBuilder::new("v2")
        .field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    assert!(encode_shadow_arrow_ipc(&schema, PanicReader, Default::default()).is_err());
}

#[test]
fn malformed_truncated_trailing_and_limits_fail_closed() {
    let schema = aura_schema();
    let arrows = arrow_schema(&schema);
    let bytes = stream(arrows.clone(), &[batch(arrows, false)]);
    for invalid in [
        bytes[..bytes.len() - 1].to_vec(),
        [&bytes[..], b"trailing"].concat(),
        vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
    ] {
        assert!(
            encode_shadow_arrow_ipc(&schema, Cursor::new(invalid), Default::default()).is_err()
        );
    }

    let too_small_input = ShadowProtocolLimits {
        max_input_bytes: bytes.len() - 1,
        max_record_batches: ShadowProtocolLimits::HARD.max_record_batches,
        values: V3ValueLimits::HARD,
    };
    assert!(encode_shadow_arrow_ipc(&schema, Cursor::new(&bytes), too_small_input).is_err());
    let row_limit = ShadowProtocolLimits {
        max_input_bytes: bytes.len(),
        max_record_batches: ShadowProtocolLimits::HARD.max_record_batches,
        values: V3ValueLimits {
            max_rows: 1,
            ..V3ValueLimits::HARD
        },
    };
    assert!(encode_shadow_arrow_ipc(&schema, Cursor::new(&bytes), row_limit).is_err());
    let value_limit = ShadowProtocolLimits {
        max_input_bytes: bytes.len(),
        max_record_batches: ShadowProtocolLimits::HARD.max_record_batches,
        values: V3ValueLimits {
            max_variable_value_bytes: 2,
            ..V3ValueLimits::HARD
        },
    };
    assert!(encode_shadow_arrow_ipc(&schema, Cursor::new(&bytes), value_limit).is_err());
    let block_limit = ShadowProtocolLimits {
        max_input_bytes: bytes.len(),
        max_record_batches: ShadowProtocolLimits::HARD.max_record_batches,
        values: V3ValueLimits {
            max_block_bytes: 64,
            ..V3ValueLimits::HARD
        },
    };
    assert!(encode_shadow_arrow_ipc(&schema, Cursor::new(&bytes), block_limit).is_err());

    let two_batches = stream(
        arrow_schema(&schema),
        &[
            batch(arrow_schema(&schema), false),
            batch(arrow_schema(&schema), true),
        ],
    );
    let batch_limit = ShadowProtocolLimits {
        max_input_bytes: two_batches.len(),
        max_record_batches: 1,
        values: V3ValueLimits::HARD,
    };
    assert!(encode_shadow_arrow_ipc(&schema, Cursor::new(two_batches), batch_limit).is_err());
}

#[test]
fn legacy_framing_and_metadata_v4_are_rejected() {
    let schema = aura_schema();
    let arrows = arrow_schema(&schema);
    let one = batch(arrows.clone(), false);
    let legacy = IpcWriteOptions::try_new(64, true, MetadataVersion::V4).unwrap();
    let legacy = stream_with_options(arrows.clone(), std::slice::from_ref(&one), legacy);
    assert!(encode_shadow_arrow_ipc(&schema, Cursor::new(legacy), Default::default()).is_err());

    for alignment in [8, 16, 32] {
        let options = IpcWriteOptions::try_new(alignment, false, MetadataVersion::V5).unwrap();
        let alternate = stream_with_options(arrows.clone(), std::slice::from_ref(&one), options);
        assert!(
            encode_shadow_arrow_ipc(&schema, Cursor::new(alternate), Default::default()).is_err()
        );
    }
}

#[test]
fn boolean_role_requires_zero_or_one() {
    let schema = SchemaBuilder::new("boolean")
        .v3()
        .field("flag", FieldType::U8, FieldRole::Boolean)
        .finish()
        .unwrap();
    let arrows = Arc::new(Schema::new(vec![Field::new(
        "flag",
        DataType::UInt8,
        false,
    )]));
    let batch =
        RecordBatch::try_new(arrows.clone(), vec![Arc::new(UInt8Array::from(vec![2]))]).unwrap();
    assert!(encode_shadow_arrow_ipc(
        &schema,
        Cursor::new(stream(arrows, &[batch])),
        Default::default()
    )
    .is_err());
}

#[test]
fn timestamp_millisecond_mapping_is_exact() {
    let schema = SchemaBuilder::new("millis")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .finish()
        .unwrap();
    let arrows = Arc::new(Schema::new(vec![Field::new(
        "ts",
        DataType::Timestamp(TimeUnit::Millisecond, None),
        false,
    )]));
    let batch = RecordBatch::try_new(
        arrows.clone(),
        vec![Arc::new(TimestampMillisecondArray::from(vec![0, i64::MAX]))],
    )
    .unwrap();
    let result = encode_shadow_arrow_ipc(
        &schema,
        Cursor::new(stream(arrows, &[batch])),
        Default::default(),
    )
    .unwrap();
    let decoded = decode_v3_value_block(&schema, &result.block, Default::default()).unwrap();
    assert_eq!(
        decoded.columns[0].value_ref(1).unwrap(),
        Some(AuraV3ValueRef::TimestampMs(i64::MAX))
    );
}

#[test]
fn multiple_timestamp_millisecond_roles_use_one_primary_marker() {
    let schema = SchemaBuilder::new("multiple_millis")
        .v3()
        .field("source_ts_ms", FieldType::TimestampMs, FieldRole::Timestamp)
        .nullable_field(
            "source_time_aux_ms",
            FieldType::TimestampMs,
            FieldRole::Timestamp,
        )
        .finish()
        .unwrap();
    assert_eq!(schema.compact_schema_map.as_deref(), Some(&[100, 255][..]));
    let arrows = Arc::new(Schema::new(vec![
        Field::new(
            "source_ts_ms",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
        Field::new(
            "source_time_aux_ms",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            true,
        ),
    ]));
    let batch = RecordBatch::try_new(
        arrows.clone(),
        vec![
            Arc::new(TimestampMillisecondArray::from(vec![1, 2])),
            Arc::new(TimestampMillisecondArray::from(vec![None, Some(i64::MAX)])),
        ],
    )
    .unwrap();
    let result = encode_shadow_arrow_ipc(
        &schema,
        Cursor::new(stream(arrows, &[batch])),
        Default::default(),
    )
    .unwrap();
    let decoded = decode_v3_value_block(&schema, &result.block, Default::default()).unwrap();
    assert_eq!(decoded.columns[1].value_ref(0).unwrap(), None);
    assert_eq!(
        decoded.columns[1].value_ref(1).unwrap(),
        Some(AuraV3ValueRef::TimestampMs(i64::MAX))
    );
}

#[test]
fn sliced_utf8_and_fixed_binary_with_nonzero_offsets_round_trip() {
    let schema = SchemaBuilder::new("slices")
        .v3()
        .field("ts", FieldType::I64, FieldRole::Timestamp)
        .nullable_field("text", FieldType::Utf8, FieldRole::Value)
        .nullable_field("opaque", FieldType::Opaque16, FieldRole::Identifier)
        .finish()
        .unwrap();
    let arrows = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("text", DataType::Utf8, true),
        Field::new("opaque", DataType::FixedSizeBinary(16), true),
    ]));
    let ts: ArrayRef = Arc::new(Int64Array::from(vec![9, 10, 11, 12, 13]).slice(1, 3));
    let text: ArrayRef = Arc::new(
        StringArray::from(vec![
            Some("prefix"),
            None,
            Some(""),
            Some("é"),
            Some("suffix"),
        ])
        .slice(1, 3),
    );
    let a = [1u8; 16];
    let b = [2u8; 16];
    let c = [3u8; 16];
    let fixed = FixedSizeBinaryArray::try_from_sparse_iter_with_size(
        [
            Some(a.as_slice()),
            None,
            Some(b.as_slice()),
            Some(c.as_slice()),
            None,
        ]
        .into_iter(),
        16,
    )
    .unwrap();
    let opaque: ArrayRef = Arc::new(fixed.slice(1, 3));
    let batch = RecordBatch::try_new(arrows.clone(), vec![ts, text, opaque]).unwrap();
    let result = encode_shadow_arrow_ipc(
        &schema,
        Cursor::new(stream(arrows, &[batch])),
        Default::default(),
    )
    .unwrap();
    let decoded = decode_v3_value_block(&schema, &result.block, Default::default()).unwrap();
    assert_eq!(decoded.columns[1].value_ref(0).unwrap(), None);
    assert_eq!(
        decoded.columns[1].value_ref(1).unwrap(),
        Some(AuraV3ValueRef::Utf8(""))
    );
    assert_eq!(
        decoded.columns[1].value_ref(2).unwrap(),
        Some(AuraV3ValueRef::Utf8("é"))
    );
    assert_eq!(decoded.columns[2].value_ref(0).unwrap(), None);
    assert_eq!(
        decoded.columns[2].value_ref(1).unwrap(),
        Some(AuraV3ValueRef::Opaque16(&b))
    );
}

#[test]
fn nullable_all_valid_and_all_null_buffers_remain_distinct() {
    let schema = SchemaBuilder::new("validity_states")
        .v3()
        .field("ts", FieldType::I64, FieldRole::Timestamp)
        .nullable_field("all_valid", FieldType::U8, FieldRole::Value)
        .nullable_field("all_null", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    let arrows = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("all_valid", DataType::UInt8, true),
        Field::new("all_null", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        arrows.clone(),
        vec![
            Arc::new(Int64Array::from(vec![1, 2])),
            Arc::new(UInt8Array::from(vec![Some(0), Some(255)])),
            Arc::new(StringArray::from(vec![None::<&str>, None])),
        ],
    )
    .unwrap();
    let result = encode_shadow_arrow_ipc(
        &schema,
        Cursor::new(stream(arrows, &[batch])),
        Default::default(),
    )
    .unwrap();
    let decoded = decode_v3_value_block(&schema, &result.block, Default::default()).unwrap();
    assert_eq!(decoded.columns[1].validity.as_deref(), Some(&[0b11][..]));
    assert_eq!(decoded.columns[2].validity.as_deref(), Some(&[0][..]));
    assert_eq!(
        decoded.columns[1].value_ref(1).unwrap(),
        Some(AuraV3ValueRef::U8(255))
    );
    assert_eq!(decoded.columns[2].value_ref(0).unwrap(), None);
}

#[test]
fn bounded_byte_mutation_and_structural_metadata_regression_never_panics() {
    let schema = SchemaBuilder::new("mutation")
        .v3()
        .field("ts", FieldType::I64, FieldRole::Timestamp)
        .finish()
        .unwrap();
    let arrows = Arc::new(Schema::new(vec![Field::new("ts", DataType::Int64, false)]));
    let batch =
        RecordBatch::try_new(arrows.clone(), vec![Arc::new(Int64Array::from(vec![42]))]).unwrap();
    let bytes = stream(arrows, &[batch]);

    for index in 0..bytes.len() {
        let mut mutated = bytes.clone();
        mutated[index] ^= 1;
        assert_no_protocol_panic(&schema, mutated);
    }
    for length in [
        0,
        1,
        4,
        8,
        bytes.len() / 2,
        bytes.len() - 9,
        bytes.len() - 1,
    ] {
        assert_no_protocol_panic(&schema, bytes[..length].to_vec());
    }

    let (node_offset, buffer_offset) = first_record_layout_offsets(&bytes);
    for (offset, value) in [
        (node_offset, 2i64),
        (node_offset + 8, -1i64),
        (buffer_offset, -1i64),
        (buffer_offset + 8, i64::MAX),
    ] {
        let mut mutated = bytes.clone();
        mutated[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        let result = std::panic::catch_unwind(|| {
            encode_shadow_arrow_ipc(&schema, Cursor::new(mutated), Default::default())
        });
        assert!(result.is_ok(), "targeted metadata mutation panicked");
        assert!(
            result.unwrap().is_err(),
            "targeted metadata mutation was accepted"
        );
    }

    let (body_start, body_len, buffers) = first_record_body_layout(&bytes);
    let mut previous_end = 0usize;
    let mut gap = None;
    for (offset, length) in buffers.iter().copied().filter(|(_, length)| *length > 0) {
        if offset > previous_end {
            gap = Some(previous_end);
            break;
        }
        previous_end = offset + length;
    }
    let gap = gap.expect("default V5 writer must contain aligned buffer padding");
    let mut gap_mutation = bytes.clone();
    gap_mutation[body_start + gap] = 1;
    assert!(
        encode_shadow_arrow_ipc(&schema, Cursor::new(gap_mutation), Default::default()).is_err()
    );
    let last_end = buffers
        .iter()
        .map(|(offset, length)| offset + length)
        .max()
        .unwrap();
    assert!(last_end < body_len);
    let mut tail_mutation = bytes.clone();
    tail_mutation[body_start + body_len - 1] = 1;
    assert!(
        encode_shadow_arrow_ipc(&schema, Cursor::new(tail_mutation), Default::default()).is_err()
    );
}

fn assert_no_protocol_panic(schema: &SchemaDescriptor, bytes: Vec<u8>) {
    let result = std::panic::catch_unwind(|| {
        encode_shadow_arrow_ipc(schema, Cursor::new(bytes), Default::default())
    });
    assert!(result.is_ok(), "single bounded mutation panicked");
}

fn first_record_layout_offsets(bytes: &[u8]) -> (usize, usize) {
    let mut position = 0usize;
    for message_index in 0..2 {
        assert_eq!(
            i32::from_le_bytes(bytes[position..position + 4].try_into().unwrap()),
            -1
        );
        let metadata_len =
            i32::from_le_bytes(bytes[position + 4..position + 8].try_into().unwrap());
        let metadata_len = usize::try_from(metadata_len).unwrap();
        let metadata_start = position + 8;
        let metadata_end = metadata_start + metadata_len;
        if message_index == 1 {
            let message = root_as_message(&bytes[metadata_start..metadata_end]).unwrap();
            let batch = message.header_as_record_batch().unwrap();
            let node = batch.nodes().unwrap().get(0);
            let buffer = batch.buffers().unwrap().get(1);
            let base = bytes.as_ptr() as usize;
            return (
                node as *const _ as usize - base,
                buffer as *const _ as usize - base,
            );
        }
        let message = root_as_message(&bytes[metadata_start..metadata_end]).unwrap();
        position = metadata_end + usize::try_from(message.bodyLength()).unwrap();
    }
    unreachable!()
}

fn first_record_body_layout(bytes: &[u8]) -> (usize, usize, Vec<(usize, usize)>) {
    let mut position = 0usize;
    for message_index in 0..2 {
        let metadata_len = usize::try_from(i32::from_le_bytes(
            bytes[position + 4..position + 8].try_into().unwrap(),
        ))
        .unwrap();
        let metadata_start = position + 8;
        let metadata_end = metadata_start + metadata_len;
        let message = root_as_message(&bytes[metadata_start..metadata_end]).unwrap();
        let body_len = usize::try_from(message.bodyLength()).unwrap();
        if message_index == 1 {
            let batch = message.header_as_record_batch().unwrap();
            let buffers = batch
                .buffers()
                .unwrap()
                .iter()
                .map(|buffer| {
                    (
                        usize::try_from(buffer.offset()).unwrap(),
                        usize::try_from(buffer.length()).unwrap(),
                    )
                })
                .collect();
            return (metadata_end, body_len, buffers);
        }
        position = metadata_end + body_len;
    }
    unreachable!()
}
