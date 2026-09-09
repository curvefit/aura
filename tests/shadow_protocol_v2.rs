use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, DictionaryArray, Int32Array, Int64Array, ListArray, StringArray, StructArray,
    TimestampMillisecondArray, UInt32Array, UInt8Array,
};
use arrow::buffer::{OffsetBuffer, ScalarBuffer};
use arrow::datatypes::{DataType, Field, Fields, Int32Type, Schema};
use arrow::ipc::writer::{IpcWriteOptions, StreamWriter};
use arrow::ipc::MetadataVersion;
use arrow::ipc::{
    root_as_message, Buffer as IpcBuffer, CompressionType, FieldNode, MessageBuilder,
    MessageHeader, RecordBatchBuilder,
};
use arrow::record_batch::RecordBatch;
use aura_codec::experimental::{
    canonical_v3_event_batch_sha256, compile_shadow_grouped_arrow_ipc,
    decode_shadow_grouped_arrow_ipc_batch, decode_v3_event_block, encode_shadow_grouped_arrow_ipc,
    shadow_grouped_arrow_protocol, AuraV3ColumnValues, ShadowGroupedProtocolLimits, V3EventLimits,
    DEFAULT_SHADOW_GROUPED_RECORD_BATCHES, SHADOW_PROTOCOL_V2, SHADOW_REPEATED_FIELD_V2,
};
use aura_codec::{FieldRole, FieldType, RelationshipPermissions, SchemaBuilder, SchemaDescriptor};

fn permissions() -> RelationshipPermissions {
    RelationshipPermissions::none()
        .with_split()
        .with_within_domain()
        .with_across_domain_same_field()
        .with_joint_same_field()
}

fn schema(okx: bool) -> SchemaDescriptor {
    let mut builder = SchemaBuilder::new(if okx {
        "grouped_arrow_v2_okx_like"
    } else {
        "grouped_arrow_v2_generic"
    })
    .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
    .nullable_field("venue_note", FieldType::Utf8, FieldRole::Identifier)
    .repeated_field("side", FieldType::U8, FieldRole::Side)
    .repeated_field("price", FieldType::I64, FieldRole::Price)
    .repeated_field("quantity", FieldType::I64, FieldRole::Quantity);
    if okx {
        builder = builder.repeated_field("order_count", FieldType::U32, FieldRole::Count);
    }
    let child_slots = if okx { vec![2, 3, 4, 5] } else { vec![2, 3, 4] };
    let mut schema = builder
        .dual_domain_repeated_group(1, child_slots, 2, permissions())
        .finish()
        .unwrap();
    schema.fields[4].nullable = true;
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

fn sliced_semantic_schema() -> SchemaDescriptor {
    let schema = SchemaBuilder::new("grouped_arrow_v2_sliced_semantics")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("exact_decimal", FieldType::DecimalText, FieldRole::Quantity)
        .dual_domain_repeated_group(1, vec![1, 2], 1, permissions())
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

fn arrow_schema(schema: &SchemaDescriptor) -> Arc<Schema> {
    let repeated = schema
        .fields
        .iter()
        .filter(|field| field.scope == aura_codec::FieldScope::Repeated)
        .map(|field| {
            let ty = match field.field_type {
                FieldType::U8 => DataType::UInt8,
                FieldType::I64 => DataType::Int64,
                FieldType::U32 => DataType::UInt32,
                _ => unreachable!(),
            };
            Field::new(&field.name, ty, field.nullable)
        })
        .collect::<Vec<_>>();
    Arc::new(Schema::new(vec![
        Field::new(
            "ts",
            DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None),
            false,
        ),
        Field::new("venue_note", DataType::Utf8, true),
        Field::new(
            SHADOW_REPEATED_FIELD_V2,
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(Fields::from(repeated)),
                false,
            ))),
            false,
        ),
    ]))
}

fn batch(
    schema: &SchemaDescriptor,
    timestamps: Vec<i64>,
    notes: Vec<Option<&str>>,
    offsets: Vec<i32>,
    side: Vec<u8>,
    price: Vec<i64>,
    quantity: Vec<Option<i64>>,
) -> RecordBatch {
    let arrows = arrow_schema(schema);
    let mut children: Vec<ArrayRef> = vec![
        Arc::new(UInt8Array::from(side)),
        Arc::new(Int64Array::from(price)),
        Arc::new(Int64Array::from(quantity)),
    ];
    if schema.fields.len() == 6 {
        children.push(Arc::new(UInt32Array::from_iter_values(
            0..children[0].len() as u32,
        )));
    }
    let item = match arrows.field(2).data_type() {
        DataType::List(item) => item.clone(),
        _ => unreachable!(),
    };
    let fields = match item.data_type() {
        DataType::Struct(fields) => fields.clone(),
        _ => unreachable!(),
    };
    let values = StructArray::new(fields, children, None);
    let list = ListArray::new(
        item,
        OffsetBuffer::new(ScalarBuffer::from(offsets)),
        Arc::new(values),
        None,
    );
    RecordBatch::try_new(
        arrows,
        vec![
            Arc::new(TimestampMillisecondArray::from(timestamps)),
            Arc::new(StringArray::from(notes)),
            Arc::new(list),
        ],
    )
    .unwrap()
}

fn stream(schema: Arc<Schema>, batches: &[RecordBatch]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let options = IpcWriteOptions::try_new(64, false, MetadataVersion::V5).unwrap();
    let mut writer = StreamWriter::try_new_with_options(&mut bytes, &schema, options).unwrap();
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
fn grouped_arrow_v2_roundtrips_generic_and_okx_like_batches() {
    assert_eq!(shadow_grouped_arrow_protocol(), SHADOW_PROTOCOL_V2);
    for schema in [schema(false), schema(true)] {
        let input = batch(
            &schema,
            vec![100, 200],
            vec![None, Some("")],
            vec![0, 1, 4],
            vec![0, 1, 0, 1],
            vec![10, 11, 12, 13],
            vec![None, Some(0), Some(5), Some(6)],
        );
        let ipc = stream(input.schema(), &[input]);
        let decoded =
            decode_shadow_grouped_arrow_ipc_batch(&schema, Cursor::new(&ipc), Default::default())
                .unwrap();
        assert_eq!(decoded.event_count, 2);
        assert_eq!(decoded.child_offsets, vec![0, 1, 4]);
        assert_eq!(
            decoded.repeated_columns[0].values,
            AuraV3ColumnValues::U8(vec![0, 1, 0, 1])
        );
        let result =
            compile_shadow_grouped_arrow_ipc(&schema, Cursor::new(&ipc), Default::default())
                .unwrap();
        assert_eq!(result.event_count, 2);
        assert_eq!(result.child_count, 4);
        assert_eq!(&result.block[..8], b"AURAV3EB");
        assert_eq!(
            result.logical_sha256,
            canonical_v3_event_batch_sha256(&schema, &decoded, V3EventLimits::default()).unwrap()
        );
        assert_eq!(
            decode_v3_event_block(&schema, &result.block, V3EventLimits::default()).unwrap(),
            decoded
        );
        let encoded =
            encode_shadow_grouped_arrow_ipc(&schema, &decoded, Default::default()).unwrap();
        assert_eq!(
            encoded,
            encode_shadow_grouped_arrow_ipc(&schema, &decoded, Default::default()).unwrap()
        );
        assert_eq!(
            decode_shadow_grouped_arrow_ipc_batch(
                &schema,
                Cursor::new(encoded),
                Default::default()
            )
            .unwrap(),
            decoded
        );
    }
}

#[test]
fn multibatch_zero_children_and_sliced_nonzero_offsets_are_exact() {
    let schema = schema(false);
    let zero = batch(&schema, vec![], vec![], vec![0], vec![], vec![], vec![]);
    let zero_decoded = decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(stream(zero.schema(), &[zero])),
        Default::default(),
    )
    .unwrap();
    assert_eq!(zero_decoded.event_count, 0);
    assert_eq!(zero_decoded.child_offsets, vec![0]);
    let first = batch(
        &schema,
        vec![1],
        vec![None],
        vec![0, 0],
        vec![],
        vec![],
        vec![],
    );
    let backing = batch(
        &schema,
        vec![9, 2, 3],
        vec![Some("skip"), Some("a"), Some("b")],
        vec![0, 1, 3, 5],
        vec![0, 0, 1, 0, 1],
        vec![9, 20, 21, 30, 31],
        vec![Some(9), Some(0), None, Some(4), Some(5)],
    );
    let sliced = backing.slice(1, 2);
    let ipc = stream(first.schema(), &[first, sliced]);
    let decoded =
        decode_shadow_grouped_arrow_ipc_batch(&schema, Cursor::new(ipc), Default::default())
            .unwrap();
    assert_eq!(decoded.event_count, 3);
    assert_eq!(decoded.child_offsets, vec![0, 0, 2, 4]);
    assert_eq!(
        decoded.repeated_columns[1].values,
        AuraV3ColumnValues::I64(vec![20, 21, 30, 31])
    );
}

#[test]
fn maximum_many_small_batches_append_without_prefix_rescanning() {
    let schema = schema(false);
    let one = batch(
        &schema,
        vec![1],
        vec![None],
        vec![0, 1],
        vec![0],
        vec![10],
        vec![None],
    );
    let batches = vec![one; DEFAULT_SHADOW_GROUPED_RECORD_BATCHES];
    let decoded = decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(stream(batches[0].schema(), &batches)),
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        usize::try_from(decoded.event_count).unwrap(),
        DEFAULT_SHADOW_GROUPED_RECORD_BATCHES
    );
    assert_eq!(
        usize::try_from(decoded.child_count()).unwrap(),
        DEFAULT_SHADOW_GROUPED_RECORD_BATCHES
    );
}

#[test]
fn sliced_unused_child_semantics_are_ignored_but_backing_utf8_remains_structural() {
    let schema = sliced_semantic_schema();
    let item = Field::new(
        "item",
        DataType::Struct(Fields::from(vec![
            Field::new("side", DataType::UInt8, false),
            Field::new("exact_decimal", DataType::Utf8, false),
        ])),
        false,
    );
    let arrow_schema = Arc::new(Schema::new(vec![
        Field::new(
            "ts",
            DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None),
            false,
        ),
        Field::new(
            SHADOW_REPEATED_FIELD_V2,
            DataType::List(Arc::new(item.clone())),
            false,
        ),
    ]));
    let ipc = raw_sliced_semantic_stream(
        arrow_schema,
        "unused-not-decimal-and-oversized",
        "1",
        "unused-bad",
    );
    let exact_block_len = compile_shadow_grouped_arrow_ipc(
        &schema,
        Cursor::new(&ipc),
        ShadowGroupedProtocolLimits::default(),
    )
    .unwrap()
    .block
    .len();
    let decoded = decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(&ipc),
        ShadowGroupedProtocolLimits {
            events: V3EventLimits {
                max_block_bytes: exact_block_len,
                max_variable_value_bytes: 1,
                ..V3EventLimits::default()
            },
            ..ShadowGroupedProtocolLimits::default()
        },
    )
    .unwrap();
    assert_eq!(decoded.event_count, 1);
    assert_eq!(decoded.child_offsets, vec![0, 1]);
    assert_eq!(
        decoded.repeated_columns[0].values,
        AuraV3ColumnValues::U8(vec![1])
    );
    assert_eq!(
        decoded.repeated_columns[1].values,
        AuraV3ColumnValues::DecimalText(aura_codec::experimental::AuraV3VariableColumn {
            offsets: vec![0, 1],
            data: b"1".to_vec(),
        })
    );

    let (body_start, _, buffers) = first_record_body_layout(&ipc);
    let decimal_data = buffers[9];
    assert!(
        decimal_data.1 > 1,
        "fixture must retain unused backing data"
    );
    let mut malformed_backing = ipc.clone();
    malformed_backing[body_start + decimal_data.0] = 0xff;
    assert!(decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(malformed_backing),
        ShadowGroupedProtocolLimits::default(),
    )
    .is_err());

    let decimal_offsets = buffers[8];
    let mut malformed_offsets = ipc;
    malformed_offsets[body_start + decimal_offsets.0 + 4..body_start + decimal_offsets.0 + 8]
        .copy_from_slice(&(-1i32).to_le_bytes());
    assert!(decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(malformed_offsets),
        ShadowGroupedProtocolLimits::default(),
    )
    .is_err());
}

#[test]
fn strict_schema_metadata_trailing_and_limits_fail_closed() {
    let schema = schema(false);
    let input = batch(
        &schema,
        vec![1],
        vec![Some("x")],
        vec![0, 1],
        vec![0],
        vec![10],
        vec![Some(0)],
    );
    let mut metadata = input.schema().as_ref().clone();
    metadata = metadata.with_metadata(std::collections::HashMap::from([(
        "forbidden".to_owned(),
        "1".to_owned(),
    )]));
    let metadata_batch =
        RecordBatch::try_new(Arc::new(metadata), input.columns().to_vec()).unwrap();
    assert!(decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(stream(metadata_batch.schema(), &[metadata_batch])),
        Default::default(),
    )
    .is_err());

    let ipc = stream(input.schema(), &[input]);
    let decoded = decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(&ipc),
        ShadowGroupedProtocolLimits::default(),
    )
    .unwrap();
    assert!(encode_shadow_grouped_arrow_ipc(
        &schema,
        &decoded,
        ShadowGroupedProtocolLimits {
            max_input_bytes: 1,
            ..ShadowGroupedProtocolLimits::default()
        },
    )
    .is_err());
    let mut trailing = ipc.clone();
    trailing.push(0);
    assert!(decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(trailing),
        Default::default()
    )
    .is_err());
    for limits in [
        ShadowGroupedProtocolLimits {
            max_input_bytes: ipc.len() - 1,
            ..Default::default()
        },
        ShadowGroupedProtocolLimits {
            events: V3EventLimits {
                max_events: 0,
                ..V3EventLimits::default()
            },
            ..Default::default()
        },
        ShadowGroupedProtocolLimits {
            events: V3EventLimits {
                max_children: 0,
                ..V3EventLimits::default()
            },
            ..Default::default()
        },
    ] {
        assert!(decode_shadow_grouped_arrow_ipc_batch(&schema, Cursor::new(&ipc), limits).is_err());
    }
}

#[test]
fn nested_metadata_dictionary_compression_and_hostile_offsets_fail_without_panic() {
    let schema = schema(false);
    let input = batch(
        &schema,
        vec![1, 2],
        vec![Some("a"), Some("b")],
        vec![0, 1, 3],
        vec![0, 1, 0],
        vec![10, 11, 12],
        vec![Some(0), None, Some(2)],
    );
    let bytes = stream(input.schema(), std::slice::from_ref(&input));
    let (body_start, body_len, buffers) = first_record_body_layout(&bytes);
    assert!(buffers.len() >= 10);
    let list_offsets = buffers[6];
    for mutation in [(0usize, -1i32), (4, 4), (8, 4)] {
        let mut hostile = bytes.clone();
        let offset = body_start + list_offsets.0 + mutation.0;
        hostile[offset..offset + 4].copy_from_slice(&mutation.1.to_le_bytes());
        let result = catch_unwind(AssertUnwindSafe(|| {
            decode_shadow_grouped_arrow_ipc_batch(&schema, Cursor::new(hostile), Default::default())
        }));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
    let (node_offset, buffer_offset) = first_record_layout_offsets(&bytes, 2, 6);
    for (offset, value) in [
        (node_offset, i64::MAX),
        (node_offset + 8, 1),
        (buffer_offset, -1),
        (buffer_offset + 8, i64::MAX),
    ] {
        let mut hostile = bytes.clone();
        hostile[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        let result = catch_unwind(AssertUnwindSafe(|| {
            decode_shadow_grouped_arrow_ipc_batch(&schema, Cursor::new(hostile), Default::default())
        }));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
    let last_end = buffers
        .iter()
        .map(|(offset, len)| offset + len)
        .max()
        .unwrap();
    assert!(last_end < body_len);
    let mut padding = bytes.clone();
    padding[body_start + body_len - 1] = 1;
    assert!(decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(padding),
        Default::default()
    )
    .is_err());

    let compressed = stream_with_options(
        input.schema(),
        std::slice::from_ref(&input),
        IpcWriteOptions::try_new(64, false, MetadataVersion::V5)
            .unwrap()
            .try_with_compression(Some(CompressionType::LZ4_FRAME))
            .unwrap(),
    );
    assert!(decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(compressed),
        Default::default()
    )
    .is_err());

    let list_field = input.schema().field(2).clone();
    let dictionary_schema = Arc::new(Schema::new(vec![
        input.schema().field(0).clone(),
        Field::new(
            "venue_note",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            true,
        ),
        list_field,
    ]));
    let dictionary = DictionaryArray::<Int32Type>::try_new(
        Int32Array::from(vec![Some(0), Some(1)]),
        Arc::new(StringArray::from(vec!["a", "b"])),
    )
    .unwrap();
    let dictionary_batch = RecordBatch::try_new(
        dictionary_schema.clone(),
        vec![
            input.column(0).clone(),
            Arc::new(dictionary),
            input.column(2).clone(),
        ],
    )
    .unwrap();
    assert!(decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(stream(dictionary_schema, &[dictionary_batch])),
        Default::default(),
    )
    .is_err());

    let input_schema = input.schema();
    let DataType::List(item) = input_schema.field(2).data_type() else {
        unreachable!()
    };
    let DataType::Struct(children) = item.data_type() else {
        unreachable!()
    };
    let mut children = children
        .iter()
        .map(|field| field.as_ref().clone())
        .collect::<Vec<_>>();
    children[1] = children[1]
        .clone()
        .with_metadata(std::collections::HashMap::from([(
            "forbidden".to_owned(),
            "1".to_owned(),
        )]));
    let metadata_schema = Arc::new(Schema::new(vec![
        input.schema().field(0).clone(),
        input.schema().field(1).clone(),
        Field::new(
            SHADOW_REPEATED_FIELD_V2,
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(Fields::from(children)),
                false,
            ))),
            false,
        ),
    ]));
    let original_list = input
        .column(2)
        .as_any()
        .downcast_ref::<ListArray>()
        .unwrap();
    let original_struct = original_list
        .values()
        .as_any()
        .downcast_ref::<StructArray>()
        .unwrap();
    let metadata_item = match metadata_schema.field(2).data_type() {
        DataType::List(item) => item.clone(),
        _ => unreachable!(),
    };
    let metadata_children = match metadata_item.data_type() {
        DataType::Struct(fields) => fields.clone(),
        _ => unreachable!(),
    };
    let metadata_struct = StructArray::new(
        metadata_children,
        original_struct.columns().to_vec(),
        original_struct.nulls().cloned(),
    );
    let metadata_list = ListArray::new(
        metadata_item,
        original_list.offsets().clone(),
        Arc::new(metadata_struct),
        original_list.nulls().cloned(),
    );
    let metadata_batch = RecordBatch::try_new(
        metadata_schema.clone(),
        vec![
            input.column(0).clone(),
            input.column(1).clone(),
            Arc::new(metadata_list),
        ],
    )
    .unwrap();
    assert!(decode_shadow_grouped_arrow_ipc_batch(
        &schema,
        Cursor::new(stream(metadata_schema, &[metadata_batch])),
        Default::default(),
    )
    .is_err());
}

#[test]
fn bounded_single_byte_mutations_never_panic() {
    let schema = schema(false);
    let input = batch(
        &schema,
        vec![1],
        vec![Some("x")],
        vec![0, 1],
        vec![0],
        vec![10],
        vec![Some(1)],
    );
    let bytes = stream(input.schema(), &[input]);
    for index in 0..bytes.len() {
        let mut hostile = bytes.clone();
        hostile[index] ^= 1;
        let result = catch_unwind(AssertUnwindSafe(|| {
            decode_shadow_grouped_arrow_ipc_batch(&schema, Cursor::new(hostile), Default::default())
        }));
        assert!(result.is_ok(), "mutation {index} panicked");
    }
}

fn raw_sliced_semantic_stream(
    schema: Arc<Schema>,
    unused_prefix: &str,
    logical: &str,
    unused_suffix: &str,
) -> Vec<u8> {
    let schema_stream = stream(schema, &[]);
    let schema_metadata_len =
        usize::try_from(i32::from_le_bytes(schema_stream[4..8].try_into().unwrap())).unwrap();
    let schema_frame_end = 8 + schema_metadata_len;

    let mut body = Vec::new();
    let mut buffers = Vec::new();
    push_ipc_buffer(&mut body, &mut buffers, &[]);
    push_ipc_buffer(&mut body, &mut buffers, &10i64.to_le_bytes());
    push_ipc_buffer(&mut body, &mut buffers, &[]);
    let mut list_offsets = Vec::new();
    list_offsets.extend_from_slice(&1i32.to_le_bytes());
    list_offsets.extend_from_slice(&2i32.to_le_bytes());
    push_ipc_buffer(&mut body, &mut buffers, &list_offsets);
    push_ipc_buffer(&mut body, &mut buffers, &[]);
    push_ipc_buffer(&mut body, &mut buffers, &[]);
    push_ipc_buffer(&mut body, &mut buffers, &[9, 1, 7]);
    push_ipc_buffer(&mut body, &mut buffers, &[]);
    let decimal_data = [
        unused_prefix.as_bytes(),
        logical.as_bytes(),
        unused_suffix.as_bytes(),
    ]
    .concat();
    let decimal_offsets = [
        0usize,
        unused_prefix.len(),
        unused_prefix.len() + logical.len(),
        decimal_data.len(),
    ];
    let mut encoded_decimal_offsets = Vec::new();
    for offset in decimal_offsets {
        encoded_decimal_offsets.extend_from_slice(&i32::try_from(offset).unwrap().to_le_bytes());
    }
    push_ipc_buffer(&mut body, &mut buffers, &encoded_decimal_offsets);
    push_ipc_buffer(&mut body, &mut buffers, &decimal_data);

    let nodes = [
        FieldNode::new(1, 0),
        FieldNode::new(1, 0),
        FieldNode::new(3, 0),
        FieldNode::new(3, 0),
        FieldNode::new(3, 0),
    ];
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let nodes = builder.create_vector(&nodes);
    let buffers = builder.create_vector(&buffers);
    let header = {
        let mut record = RecordBatchBuilder::new(&mut builder);
        record.add_length(1);
        record.add_nodes(nodes);
        record.add_buffers(buffers);
        record.finish().as_union_value()
    };
    let message = {
        let mut message = MessageBuilder::new(&mut builder);
        message.add_version(MetadataVersion::V5);
        message.add_header_type(MessageHeader::RecordBatch);
        message.add_bodyLength(i64::try_from(body.len()).unwrap());
        message.add_header(header);
        message.finish()
    };
    builder.finish(message, None);

    let metadata = builder.finished_data();
    let metadata_padding = (64 - ((8 + metadata.len()) % 64)) % 64;
    let metadata_len = metadata.len() + metadata_padding;
    let mut output = Vec::new();
    output.extend_from_slice(&schema_stream[..schema_frame_end]);
    output.extend_from_slice(&(-1i32).to_le_bytes());
    output.extend_from_slice(&i32::try_from(metadata_len).unwrap().to_le_bytes());
    output.extend_from_slice(metadata);
    output.resize(output.len() + metadata_padding, 0);
    output.extend_from_slice(&body);
    output.extend_from_slice(&(-1i32).to_le_bytes());
    output.extend_from_slice(&0i32.to_le_bytes());
    output
}

fn push_ipc_buffer(body: &mut Vec<u8>, buffers: &mut Vec<IpcBuffer>, bytes: &[u8]) {
    buffers.push(IpcBuffer::new(
        i64::try_from(body.len()).unwrap(),
        i64::try_from(bytes.len()).unwrap(),
    ));
    body.extend_from_slice(bytes);
    let padding = (64 - (body.len() % 64)) % 64;
    body.resize(body.len() + padding, 0);
}

fn first_record_layout_offsets(
    bytes: &[u8],
    node_index: usize,
    buffer_index: usize,
) -> (usize, usize) {
    let mut position = 0usize;
    for message_index in 0..2 {
        let metadata_len = usize::try_from(i32::from_le_bytes(
            bytes[position + 4..position + 8].try_into().unwrap(),
        ))
        .unwrap();
        let start = position + 8;
        let end = start + metadata_len;
        let message = root_as_message(&bytes[start..end]).unwrap();
        if message_index == 1 {
            let batch = message.header_as_record_batch().unwrap();
            let node = batch.nodes().unwrap().get(node_index);
            let buffer = batch.buffers().unwrap().get(buffer_index);
            let base = bytes.as_ptr() as usize;
            return (
                node as *const _ as usize - base,
                buffer as *const _ as usize - base,
            );
        }
        position = end + usize::try_from(message.bodyLength()).unwrap();
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
        let start = position + 8;
        let end = start + metadata_len;
        let message = root_as_message(&bytes[start..end]).unwrap();
        let body_len = usize::try_from(message.bodyLength()).unwrap();
        if message_index == 1 {
            let batch = message.header_as_record_batch().unwrap();
            return (
                end,
                body_len,
                batch
                    .buffers()
                    .unwrap()
                    .iter()
                    .map(|buffer| {
                        (
                            usize::try_from(buffer.offset()).unwrap(),
                            usize::try_from(buffer.length()).unwrap(),
                        )
                    })
                    .collect(),
            );
        }
        position = end + body_len;
    }
    unreachable!()
}
