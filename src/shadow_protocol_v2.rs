//! Strict provider-independent Arrow IPC boundary for grouped V3 event blocks.
//!
//! `aura-logical-arrow-ipc-v2` transports exactly one grouped logical batch as
//! event-scoped top-level columns followed by one reserved nested repeated
//! column. Its output is a standalone `AURAV3EB` reference block, never an
//! Aura0 container.

use std::io::{self, Cursor, Read, Write};
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, FixedSizeBinaryArray, Int16Array, Int32Array, Int64Array, Int8Array,
    ListArray, StringArray, StructArray, TimestampMillisecondArray, TimestampNanosecondArray,
    UInt16Array, UInt32Array, UInt64Array, UInt8Array,
};
use arrow::buffer::{OffsetBuffer, ScalarBuffer};
use arrow::datatypes::{DataType, Field, Fields, Schema};
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::{IpcWriteOptions, StreamWriter};
use arrow::ipc::{root_as_message, Endianness, MessageHeader, MetadataVersion};
use arrow::record_batch::RecordBatch;
use sha2::{Digest, Sha256};

use crate::schema::{FieldDescriptor, FieldRole, FieldScope, FieldType, SchemaDescriptor};
use crate::shadow_protocol::{
    align_64, append_validity, append_value, arrow_type, downcast, fixed_width, ipc_buffer,
    ipc_field_type_matches, read_bounded, reserve_column, take_i32, validate_ipc_validity,
    SHADOW_SCHEMA_FORMAT,
};
use crate::v3_events::{
    canonical_v3_event_batch_sha256, encode_v3_event_block, validate_v3_event_batch,
    validate_v3_grouped_exact_subset, AuraV3EventBatch, V3EventLimits, MAX_V3_EVENT_BLOCK_BYTES,
    MAX_V3_EVENT_CHILDREN, MAX_V3_EVENT_EVENTS, MAX_V3_EVENT_VALUES,
};
use crate::v3_values::{
    canonical_v3_schema_fingerprint, is_present, validate_decimal_text_v1, AuraV3Column,
    AuraV3ColumnValues, AuraV3VariableColumn, MAX_V3_VARIABLE_VALUE_BYTES,
};
use crate::{AuraError, Result};

pub const SHADOW_PROTOCOL_V2: &str = "aura-logical-arrow-ipc-v2";
pub const SHADOW_ARTIFACT_KIND_V2: &str = "standalone-aura-v3-event-block-v1";
pub const SHADOW_RESULT_SCHEMA_V2: &str = "aura-shadow-grouped-encode-result-v1";
pub const SHADOW_VERIFY_RESULT_SCHEMA_V2: &str = "aura-shadow-grouped-verify-result-v1";
pub const SHADOW_REPEATED_FIELD_V2: &str = "__aura_repeated_v1";

pub const MAX_SHADOW_GROUPED_ARROW_IPC_BYTES: usize = 1024 * 1024 * 1024;
pub const DEFAULT_SHADOW_GROUPED_ARROW_IPC_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_SHADOW_GROUPED_RECORD_BATCHES: usize = 65_536;
pub const DEFAULT_SHADOW_GROUPED_RECORD_BATCHES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowGroupedProtocolLimits {
    pub max_input_bytes: usize,
    pub max_record_batches: usize,
    pub events: V3EventLimits,
}

impl ShadowGroupedProtocolLimits {
    pub const HARD: Self = Self {
        max_input_bytes: MAX_SHADOW_GROUPED_ARROW_IPC_BYTES,
        max_record_batches: MAX_SHADOW_GROUPED_RECORD_BATCHES,
        events: V3EventLimits::HARD,
    };

    pub const DEFAULT: Self = Self {
        max_input_bytes: DEFAULT_SHADOW_GROUPED_ARROW_IPC_BYTES,
        max_record_batches: DEFAULT_SHADOW_GROUPED_RECORD_BATCHES,
        events: V3EventLimits::DEFAULT_IN_MEMORY,
    };

    const fn effective(self) -> Self {
        Self {
            max_input_bytes: min_usize(self.max_input_bytes, MAX_SHADOW_GROUPED_ARROW_IPC_BYTES),
            max_record_batches: min_usize(
                self.max_record_batches,
                MAX_SHADOW_GROUPED_RECORD_BATCHES,
            ),
            events: V3EventLimits {
                max_block_bytes: min_usize(self.events.max_block_bytes, MAX_V3_EVENT_BLOCK_BYTES),
                max_variable_value_bytes: min_usize(
                    self.events.max_variable_value_bytes,
                    MAX_V3_VARIABLE_VALUE_BYTES,
                ),
                max_events: min_usize(self.events.max_events, MAX_V3_EVENT_EVENTS),
                max_children: min_usize(self.events.max_children, MAX_V3_EVENT_CHILDREN),
                max_values: min_usize(self.events.max_values, MAX_V3_EVENT_VALUES),
            },
        }
    }
}

impl Default for ShadowGroupedProtocolLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

const fn min_usize(left: usize, right: usize) -> usize {
    if left < right {
        left
    } else {
        right
    }
}

struct BoundedIpcWriter {
    bytes: Vec<u8>,
    limit: usize,
    length_exceeded: bool,
    allocation_failed: bool,
}

impl BoundedIpcWriter {
    fn new(limit: usize) -> Result<Self> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve(limit.min(64 * 1024))
            .map_err(|_| AuraError::InvalidValue("shadow grouped ipc allocation"))?;
        Ok(Self {
            bytes,
            limit,
            length_exceeded: false,
            allocation_failed: false,
        })
    }

    fn aura_error(&self) -> AuraError {
        if self.length_exceeded {
            AuraError::InvalidValue("shadow grouped ipc input length")
        } else if self.allocation_failed {
            AuraError::InvalidValue("shadow grouped ipc allocation")
        } else {
            AuraError::InvalidValue("shadow grouped ipc encode")
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedIpcWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(end) = self.bytes.len().checked_add(bytes.len()) else {
            self.length_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "shadow grouped ipc input length",
            ));
        };
        if end > self.limit {
            self.length_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "shadow grouped ipc input length",
            ));
        }
        if self.bytes.try_reserve(bytes.len()).is_err() {
            self.allocation_failed = true;
            return Err(io::Error::other("shadow grouped ipc allocation"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowGroupedEncodeResult {
    pub schema_fingerprint: [u8; 32],
    pub event_count: u32,
    pub child_count: u32,
    pub logical_sha256: [u8; 32],
    pub block: Vec<u8>,
    pub block_sha256: [u8; 32],
}

/// Compile one strict grouped Arrow stream into a standalone exact-event block.
pub fn compile_shadow_grouped_arrow_ipc<R: Read>(
    schema: &SchemaDescriptor,
    input: R,
    limits: ShadowGroupedProtocolLimits,
) -> Result<ShadowGroupedEncodeResult> {
    let limits = limits.effective();
    let batch = decode_shadow_grouped_arrow_ipc_batch(schema, input, limits)?;
    let schema_fingerprint = canonical_v3_schema_fingerprint(schema)?;
    let logical_sha256 = canonical_v3_event_batch_sha256(schema, &batch, limits.events)?;
    let block = encode_v3_event_block(schema, &batch, limits.events)?;
    let block_sha256 = Sha256::digest(&block).into();
    Ok(ShadowGroupedEncodeResult {
        schema_fingerprint,
        event_count: batch.event_count,
        child_count: batch.child_count(),
        logical_sha256,
        block,
        block_sha256,
    })
}

/// Decode one strict grouped Arrow stream into Aura's exact grouped batch.
pub fn decode_shadow_grouped_arrow_ipc_batch<R: Read>(
    schema: &SchemaDescriptor,
    mut input: R,
    limits: ShadowGroupedProtocolLimits,
) -> Result<AuraV3EventBatch> {
    let limits = limits.effective();
    validate_v3_grouped_exact_subset(schema)?;
    let ipc = read_bounded(&mut input, limits.max_input_bytes)?;
    preflight_grouped_ipc_stream(schema, &ipc, limits)?;
    let batch = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decode_grouped_ipc_batches(schema, &ipc, limits)
    }))
    .map_err(|_| AuraError::InvalidValue("shadow grouped arrow ipc panic"))??;
    validate_v3_event_batch(schema, &batch, limits.events)?;
    Ok(batch)
}

/// Deterministically encode an exact grouped batch as Arrow 54 IPC V5.
pub fn encode_shadow_grouped_arrow_ipc(
    schema: &SchemaDescriptor,
    batch: &AuraV3EventBatch,
    limits: ShadowGroupedProtocolLimits,
) -> Result<Vec<u8>> {
    let limits = limits.effective();
    validate_v3_grouped_exact_subset(schema)?;
    validate_v3_event_batch(schema, batch, limits.events)?;
    let arrow_schema = Arc::new(grouped_arrow_schema(schema)?);
    let record_batch = grouped_record_batch(schema, batch, arrow_schema.clone())?;
    let options = IpcWriteOptions::try_new(64, false, MetadataVersion::V5)
        .map_err(|_| AuraError::InvalidValue("shadow grouped ipc options"))?;
    let mut output = BoundedIpcWriter::new(limits.max_input_bytes)?;
    let encoded = (|| {
        let mut writer = StreamWriter::try_new_with_options(&mut output, &arrow_schema, options)?;
        writer.write(&record_batch)?;
        writer.finish()
    })();
    if encoded.is_err() {
        return Err(output.aura_error());
    }
    let bytes = output.into_inner();
    preflight_grouped_ipc_stream(schema, &bytes, limits)?;
    Ok(bytes)
}

pub const fn shadow_grouped_arrow_protocol() -> &'static str {
    SHADOW_PROTOCOL_V2
}

pub const fn shadow_grouped_schema_format() -> &'static str {
    SHADOW_SCHEMA_FORMAT
}

fn scoped_fields(schema: &SchemaDescriptor) -> (Vec<&FieldDescriptor>, Vec<&FieldDescriptor>) {
    let mut event = Vec::new();
    let mut repeated = Vec::new();
    for field in &schema.fields {
        match field.scope {
            FieldScope::Event => event.push(field),
            FieldScope::Repeated => repeated.push(field),
        }
    }
    (event, repeated)
}

fn grouped_arrow_schema(schema: &SchemaDescriptor) -> Result<Schema> {
    let (event_fields, repeated_fields) = scoped_fields(schema);
    if schema
        .fields
        .iter()
        .any(|field| field.name == SHADOW_REPEATED_FIELD_V2)
    {
        return Err(AuraError::InvalidValue("shadow grouped reserved field"));
    }
    let mut fields = Vec::new();
    fields
        .try_reserve_exact(event_fields.len() + 1)
        .map_err(|_| AuraError::InvalidValue("shadow grouped schema allocation"))?;
    for field in event_fields {
        fields.push(Field::new(
            &field.name,
            arrow_type(field.field_type),
            field.nullable,
        ));
    }
    let children = repeated_fields
        .into_iter()
        .map(|field| Field::new(&field.name, arrow_type(field.field_type), field.nullable))
        .collect::<Vec<_>>();
    let item = Field::new("item", DataType::Struct(Fields::from(children)), false);
    fields.push(Field::new(
        SHADOW_REPEATED_FIELD_V2,
        DataType::List(Arc::new(item)),
        false,
    ));
    Ok(Schema::new(fields))
}

fn grouped_record_batch(
    schema: &SchemaDescriptor,
    batch: &AuraV3EventBatch,
    arrow_schema: Arc<Schema>,
) -> Result<RecordBatch> {
    let (event_fields, repeated_fields) = scoped_fields(schema);
    let mut arrays = Vec::new();
    arrays
        .try_reserve_exact(event_fields.len() + 1)
        .map_err(|_| AuraError::InvalidValue("shadow grouped array allocation"))?;
    for (field, column) in event_fields.iter().zip(&batch.event_columns) {
        arrays.push(column_to_array(field, column)?);
    }
    let mut child_arrays = Vec::new();
    child_arrays
        .try_reserve_exact(repeated_fields.len())
        .map_err(|_| AuraError::InvalidValue("shadow grouped array allocation"))?;
    for (field, column) in repeated_fields.iter().zip(&batch.repeated_columns) {
        child_arrays.push(column_to_array(field, column)?);
    }
    let list_field = arrow_schema.field(event_fields.len()).clone();
    let DataType::List(item) = list_field.data_type() else {
        return Err(AuraError::InvalidValue("shadow grouped list schema"));
    };
    let DataType::Struct(child_fields) = item.data_type() else {
        return Err(AuraError::InvalidValue("shadow grouped struct schema"));
    };
    let struct_array = StructArray::new(child_fields.clone(), child_arrays, None);
    let offsets = batch
        .child_offsets
        .iter()
        .map(|offset| {
            i32::try_from(*offset).map_err(|_| AuraError::InvalidValue("shadow grouped offsets"))
        })
        .collect::<Result<Vec<_>>>()?;
    let offsets = OffsetBuffer::new(ScalarBuffer::from(offsets));
    arrays.push(Arc::new(ListArray::new(
        item.clone(),
        offsets,
        Arc::new(struct_array),
        None,
    )));
    RecordBatch::try_new(arrow_schema, arrays)
        .map_err(|_| AuraError::InvalidValue("shadow grouped record batch"))
}

fn column_to_array(field: &FieldDescriptor, column: &AuraV3Column) -> Result<ArrayRef> {
    let rows = column_len(&column.values);
    let present = |row: usize| is_present(column.validity.as_deref(), row);
    macro_rules! primitive {
        ($values:expr, $array:ty) => {{
            let values = (0..rows)
                .map(|row| present(row).then_some($values[row]))
                .collect::<Vec<_>>();
            Arc::new(<$array>::from(values)) as ArrayRef
        }};
    }
    let array = match &column.values {
        AuraV3ColumnValues::I8(values) => primitive!(values, Int8Array),
        AuraV3ColumnValues::U8(values) => primitive!(values, UInt8Array),
        AuraV3ColumnValues::I16(values) => primitive!(values, Int16Array),
        AuraV3ColumnValues::U16(values) => primitive!(values, UInt16Array),
        AuraV3ColumnValues::I32(values) => primitive!(values, Int32Array),
        AuraV3ColumnValues::U32(values) => primitive!(values, UInt32Array),
        AuraV3ColumnValues::I64(values) => primitive!(values, Int64Array),
        AuraV3ColumnValues::U64(values) => primitive!(values, UInt64Array),
        AuraV3ColumnValues::TimestampNs(values) => primitive!(values, TimestampNanosecondArray),
        AuraV3ColumnValues::TimestampMs(values) => {
            primitive!(values, TimestampMillisecondArray)
        }
        AuraV3ColumnValues::I128(values) => {
            let owned = (0..rows)
                .map(|row| present(row).then(|| values[row].to_le_bytes()))
                .collect::<Vec<_>>();
            let iter = owned
                .iter()
                .map(|value| value.as_ref().map(|value| value.as_slice()));
            Arc::new(
                FixedSizeBinaryArray::try_from_sparse_iter_with_size(iter, 16)
                    .map_err(|_| AuraError::InvalidValue("shadow grouped fixed binary"))?,
            )
        }
        AuraV3ColumnValues::Opaque16(values) => {
            let iter = values
                .iter()
                .enumerate()
                .map(|(row, value)| present(row).then_some(value.as_slice()));
            Arc::new(
                FixedSizeBinaryArray::try_from_sparse_iter_with_size(iter, 16)
                    .map_err(|_| AuraError::InvalidValue("shadow grouped fixed binary"))?,
            )
        }
        AuraV3ColumnValues::Utf8(variable) | AuraV3ColumnValues::DecimalText(variable) => {
            let mut values = Vec::new();
            values
                .try_reserve_exact(rows)
                .map_err(|_| AuraError::InvalidValue("shadow grouped string allocation"))?;
            for row in 0..rows {
                if !present(row) {
                    values.push(None);
                    continue;
                }
                let start = usize::try_from(variable.offsets[row])
                    .map_err(|_| AuraError::InvalidValue("shadow grouped string offset"))?;
                let end = usize::try_from(variable.offsets[row + 1])
                    .map_err(|_| AuraError::InvalidValue("shadow grouped string offset"))?;
                let value = std::str::from_utf8(
                    variable
                        .data
                        .get(start..end)
                        .ok_or(AuraError::InvalidValue("shadow grouped string offset"))?,
                )
                .map_err(|_| AuraError::InvalidValue("shadow grouped utf8"))?;
                values.push(Some(value));
            }
            Arc::new(StringArray::from(values))
        }
    };
    if array.data_type() != &arrow_type(field.field_type) {
        return Err(AuraError::InvalidValue("shadow grouped arrow type"));
    }
    Ok(array)
}

fn column_len(values: &AuraV3ColumnValues) -> usize {
    match values {
        AuraV3ColumnValues::I8(values) => values.len(),
        AuraV3ColumnValues::U8(values) => values.len(),
        AuraV3ColumnValues::I16(values) => values.len(),
        AuraV3ColumnValues::U16(values) => values.len(),
        AuraV3ColumnValues::I32(values) => values.len(),
        AuraV3ColumnValues::U32(values) => values.len(),
        AuraV3ColumnValues::I64(values) => values.len(),
        AuraV3ColumnValues::U64(values) => values.len(),
        AuraV3ColumnValues::TimestampNs(values) => values.len(),
        AuraV3ColumnValues::TimestampMs(values) => values.len(),
        AuraV3ColumnValues::I128(values) => values.len(),
        AuraV3ColumnValues::Opaque16(values) => values.len(),
        AuraV3ColumnValues::Utf8(values) | AuraV3ColumnValues::DecimalText(values) => {
            values.offsets.len().saturating_sub(1)
        }
    }
}

fn decode_grouped_ipc_batches(
    schema: &SchemaDescriptor,
    ipc: &[u8],
    limits: ShadowGroupedProtocolLimits,
) -> Result<AuraV3EventBatch> {
    let mut output = empty_event_batch(schema)?;
    let mut reader = StreamReader::try_new(Cursor::new(ipc), None)
        .map_err(|_| AuraError::InvalidValue("shadow grouped arrow ipc"))?;
    validate_grouped_arrow_schema(schema, reader.schema().as_ref())?;
    for batch in &mut reader {
        let batch = batch.map_err(|_| AuraError::InvalidValue("shadow grouped arrow ipc"))?;
        append_grouped_record_batch(schema, &mut output, &batch, limits)?;
    }
    Ok(output)
}

fn empty_event_batch(schema: &SchemaDescriptor) -> Result<AuraV3EventBatch> {
    let (event_fields, repeated_fields) = scoped_fields(schema);
    let mut event_columns = Vec::new();
    event_columns
        .try_reserve_exact(event_fields.len())
        .map_err(|_| AuraError::InvalidValue("shadow grouped column allocation"))?;
    for field in event_fields {
        event_columns.push(empty_column(field)?);
    }
    let mut repeated_columns = Vec::new();
    repeated_columns
        .try_reserve_exact(repeated_fields.len())
        .map_err(|_| AuraError::InvalidValue("shadow grouped column allocation"))?;
    for field in repeated_fields {
        repeated_columns.push(empty_column(field)?);
    }
    Ok(AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 0,
        child_offsets: vec![0],
        event_columns,
        repeated_columns,
    })
}

fn empty_column(field: &FieldDescriptor) -> Result<AuraV3Column> {
    let values = match field.field_type {
        FieldType::I8 => AuraV3ColumnValues::I8(Vec::new()),
        FieldType::U8 => AuraV3ColumnValues::U8(Vec::new()),
        FieldType::I16 => AuraV3ColumnValues::I16(Vec::new()),
        FieldType::U16 => AuraV3ColumnValues::U16(Vec::new()),
        FieldType::I32 => AuraV3ColumnValues::I32(Vec::new()),
        FieldType::U32 => AuraV3ColumnValues::U32(Vec::new()),
        FieldType::I64 => AuraV3ColumnValues::I64(Vec::new()),
        FieldType::U64 => AuraV3ColumnValues::U64(Vec::new()),
        FieldType::TimestampNs => AuraV3ColumnValues::TimestampNs(Vec::new()),
        FieldType::TimestampMs => AuraV3ColumnValues::TimestampMs(Vec::new()),
        FieldType::I128 => AuraV3ColumnValues::I128(Vec::new()),
        FieldType::Opaque16 => AuraV3ColumnValues::Opaque16(Vec::new()),
        FieldType::Utf8 | FieldType::DecimalText => {
            let variable = AuraV3VariableColumn {
                offsets: vec![0],
                data: Vec::new(),
            };
            if field.field_type == FieldType::Utf8 {
                AuraV3ColumnValues::Utf8(variable)
            } else {
                AuraV3ColumnValues::DecimalText(variable)
            }
        }
    };
    Ok(AuraV3Column {
        slot: field.index,
        validity: field.nullable.then(Vec::new),
        values,
    })
}

fn append_grouped_record_batch(
    schema: &SchemaDescriptor,
    output: &mut AuraV3EventBatch,
    input: &RecordBatch,
    limits: ShadowGroupedProtocolLimits,
) -> Result<()> {
    validate_grouped_arrow_schema(schema, input.schema().as_ref())?;
    let (event_fields, repeated_fields) = scoped_fields(schema);
    let old_events = usize::try_from(output.event_count)
        .map_err(|_| AuraError::InvalidValue("shadow grouped event count"))?;
    let new_events = old_events
        .checked_add(input.num_rows())
        .filter(|count| *count <= limits.events.max_events)
        .ok_or(AuraError::InvalidValue("shadow grouped event count"))?;
    for (((field, column), array), _) in event_fields
        .iter()
        .zip(&mut output.event_columns)
        .zip(input.columns())
        .zip(0..event_fields.len())
    {
        append_array(field, column, array.as_ref(), 0, input.num_rows())?;
    }
    let list: &ListArray = downcast(input.column(event_fields.len()).as_ref())?;
    if list.null_count() != 0 {
        return Err(AuraError::InvalidValue("shadow grouped list null"));
    }
    let offsets = list.value_offsets();
    if offsets.len() != input.num_rows() + 1 {
        return Err(AuraError::InvalidValue("shadow grouped offsets"));
    }
    let base = usize::try_from(offsets[0])
        .map_err(|_| AuraError::InvalidValue("shadow grouped offsets"))?;
    let end = usize::try_from(*offsets.last().unwrap_or(&offsets[0]))
        .map_err(|_| AuraError::InvalidValue("shadow grouped offsets"))?;
    if end < base {
        return Err(AuraError::InvalidValue("shadow grouped offsets"));
    }
    let child_count = end - base;
    let old_children = usize::try_from(output.child_count())
        .map_err(|_| AuraError::InvalidValue("shadow grouped child count"))?;
    let new_children = old_children
        .checked_add(child_count)
        .filter(|count| *count <= limits.events.max_children)
        .ok_or(AuraError::InvalidValue("shadow grouped child count"))?;
    let struct_values: &StructArray = downcast(list.values().as_ref())?;
    if end > struct_values.len() || struct_values.null_count() != 0 {
        return Err(AuraError::InvalidValue("shadow grouped struct values"));
    }
    if struct_values.columns().len() != repeated_fields.len() {
        return Err(AuraError::InvalidValue("shadow grouped child columns"));
    }
    for ((field, column), array) in repeated_fields
        .iter()
        .zip(&mut output.repeated_columns)
        .zip(struct_values.columns())
    {
        append_array(field, column, array.as_ref(), base, child_count)?;
    }
    for offset in offsets.iter().skip(1) {
        let local = usize::try_from(*offset)
            .map_err(|_| AuraError::InvalidValue("shadow grouped offsets"))?;
        if local < base || local > end {
            return Err(AuraError::InvalidValue("shadow grouped offsets"));
        }
        output.child_offsets.push(
            u32::try_from(old_children + (local - base))
                .map_err(|_| AuraError::InvalidValue("shadow grouped child count"))?,
        );
    }
    output.event_count = u32::try_from(new_events)
        .map_err(|_| AuraError::InvalidValue("shadow grouped event count"))?;
    if usize::try_from(output.child_count())
        .map_err(|_| AuraError::InvalidValue("shadow grouped child count"))?
        != new_children
    {
        return Err(AuraError::InvalidValue("shadow grouped child count"));
    }
    Ok(())
}

fn append_array(
    field: &FieldDescriptor,
    output: &mut AuraV3Column,
    input: &dyn Array,
    start: usize,
    len: usize,
) -> Result<()> {
    let end = start
        .checked_add(len)
        .filter(|end| *end <= input.len())
        .ok_or(AuraError::InvalidValue("shadow grouped array range"))?;
    let sliced = input.slice(start, len);
    let old_rows = column_len(&output.values);
    reserve_column(output, sliced.as_ref(), len, field.nullable)?;
    for row in 0..len {
        let present = !sliced.is_null(row);
        if !field.nullable && !present {
            return Err(AuraError::InvalidValue("shadow grouped nonnullable null"));
        }
        if let Some(validity) = &mut output.validity {
            append_validity(validity, old_rows + row, present)?;
        }
        append_value(field, &mut output.values, sliced.as_ref(), row, present)?;
    }
    debug_assert_eq!(end, start + len);
    Ok(())
}

fn validate_grouped_arrow_schema(expected: &SchemaDescriptor, actual: &Schema) -> Result<()> {
    let expected_schema = grouped_arrow_schema(expected)?;
    if !actual.metadata().is_empty() || actual.fields() != expected_schema.fields() {
        return Err(AuraError::InvalidValue("shadow grouped arrow schema"));
    }
    Ok(())
}

fn preflight_grouped_ipc_stream(
    schema: &SchemaDescriptor,
    bytes: &[u8],
    limits: ShadowGroupedProtocolLimits,
) -> Result<()> {
    validate_v3_grouped_exact_subset(schema)?;
    let mut position = 0usize;
    let mut message_index = 0usize;
    let mut batch_count = 0usize;
    let mut total_events = 0usize;
    let mut total_children = 0usize;
    let mut variable_bytes = 0usize;
    loop {
        if take_i32(bytes, &mut position)? != -1 {
            return Err(AuraError::InvalidValue(
                "shadow grouped ipc continuation marker",
            ));
        }
        let metadata_len = take_i32(bytes, &mut position)?;
        if metadata_len == 0 {
            if message_index == 0 || position != bytes.len() {
                return Err(AuraError::InvalidValue("shadow grouped ipc eos"));
            }
            return Ok(());
        }
        let metadata_len = usize::try_from(metadata_len)
            .map_err(|_| AuraError::InvalidValue("shadow grouped ipc metadata length"))?;
        if metadata_len
            .checked_add(8)
            .is_none_or(|framed| framed % 64 != 0)
        {
            return Err(AuraError::InvalidValue(
                "shadow grouped ipc metadata alignment",
            ));
        }
        let metadata_end = position
            .checked_add(metadata_len)
            .ok_or(AuraError::InvalidValue(
                "shadow grouped ipc metadata length",
            ))?;
        let metadata = bytes
            .get(position..metadata_end)
            .ok_or(AuraError::UnexpectedEof)?;
        position = metadata_end;
        let message = root_as_message(metadata)
            .map_err(|_| AuraError::InvalidValue("shadow grouped ipc metadata"))?;
        if message.version() != MetadataVersion::V5
            || message
                .custom_metadata()
                .is_some_and(|metadata| !metadata.is_empty())
        {
            return Err(AuraError::InvalidValue(
                "shadow grouped ipc metadata version",
            ));
        }
        let expected = if message_index == 0 {
            MessageHeader::Schema
        } else {
            MessageHeader::RecordBatch
        };
        if message.header_type() != expected {
            return Err(AuraError::InvalidValue("shadow grouped ipc message type"));
        }
        let body_len = usize::try_from(message.bodyLength())
            .map_err(|_| AuraError::InvalidValue("shadow grouped ipc body length"))?;
        if body_len % 64 != 0 {
            return Err(AuraError::InvalidValue("shadow grouped ipc body alignment"));
        }
        let body_end = position
            .checked_add(body_len)
            .filter(|end| *end <= bytes.len())
            .ok_or(AuraError::UnexpectedEof)?;
        let body = &bytes[position..body_end];
        if message_index == 0 {
            let ipc_schema = message
                .header_as_schema()
                .ok_or(AuraError::InvalidValue("shadow grouped ipc schema"))?;
            if ipc_schema.endianness() != Endianness::Little || !body.is_empty() {
                return Err(AuraError::InvalidValue("shadow grouped ipc endianness"));
            }
            validate_grouped_ipc_schema(schema, ipc_schema)?;
        } else {
            let record_batch = message
                .header_as_record_batch()
                .ok_or(AuraError::InvalidValue("shadow grouped ipc record batch"))?;
            if record_batch.compression().is_some() {
                return Err(AuraError::InvalidValue("shadow grouped ipc compression"));
            }
            batch_count = batch_count
                .checked_add(1)
                .filter(|count| *count <= limits.max_record_batches)
                .ok_or(AuraError::InvalidValue("shadow grouped ipc batch count"))?;
            preflight_grouped_record_batch(
                schema,
                record_batch,
                body,
                &mut total_events,
                &mut total_children,
                &mut variable_bytes,
                limits,
            )?;
        }
        position = body_end;
        message_index = message_index
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("shadow grouped ipc message count"))?;
    }
}

fn validate_grouped_ipc_schema(
    expected: &SchemaDescriptor,
    actual: arrow::ipc::Schema<'_>,
) -> Result<()> {
    if actual
        .custom_metadata()
        .is_some_and(|metadata| !metadata.is_empty())
        || actual
            .features()
            .is_some_and(|features| !features.is_empty())
    {
        return Err(AuraError::InvalidValue(
            "shadow grouped ipc schema metadata",
        ));
    }
    let (event_fields, repeated_fields) = scoped_fields(expected);
    let fields = actual
        .fields()
        .ok_or(AuraError::InvalidValue("shadow grouped ipc schema fields"))?;
    if fields.len() != event_fields.len() + 1 {
        return Err(AuraError::InvalidValue("shadow grouped ipc schema fields"));
    }
    for (index, field) in event_fields.iter().enumerate() {
        validate_ipc_scalar_field(field, fields.get(index))?;
    }
    let repeated = fields.get(event_fields.len());
    if repeated.name() != Some(SHADOW_REPEATED_FIELD_V2)
        || repeated.nullable()
        || repeated.dictionary().is_some()
        || repeated
            .custom_metadata()
            .is_some_and(|metadata| !metadata.is_empty())
        || repeated.type_type() != arrow::ipc::Type::List
        || repeated.type_as_list().is_none()
    {
        return Err(AuraError::InvalidValue("shadow grouped ipc list field"));
    }
    let list_children = repeated
        .children()
        .ok_or(AuraError::InvalidValue("shadow grouped ipc list child"))?;
    if list_children.len() != 1 {
        return Err(AuraError::InvalidValue("shadow grouped ipc list child"));
    }
    let item = list_children.get(0);
    if item.name() != Some("item")
        || item.nullable()
        || item.dictionary().is_some()
        || item
            .custom_metadata()
            .is_some_and(|metadata| !metadata.is_empty())
        || item.type_type() != arrow::ipc::Type::Struct_
        || item.type_as_struct_().is_none()
    {
        return Err(AuraError::InvalidValue("shadow grouped ipc struct field"));
    }
    let children = item.children().ok_or(AuraError::InvalidValue(
        "shadow grouped ipc struct children",
    ))?;
    if children.len() != repeated_fields.len() {
        return Err(AuraError::InvalidValue(
            "shadow grouped ipc struct children",
        ));
    }
    for (index, field) in repeated_fields.iter().enumerate() {
        validate_ipc_scalar_field(field, children.get(index))?;
    }
    Ok(())
}

fn validate_ipc_scalar_field(
    expected: &FieldDescriptor,
    actual: arrow::ipc::Field<'_>,
) -> Result<()> {
    if actual.name() != Some(expected.name.as_str())
        || actual.nullable() != expected.nullable
        || actual.dictionary().is_some()
        || actual
            .children()
            .is_some_and(|children| !children.is_empty())
        || actual
            .custom_metadata()
            .is_some_and(|metadata| !metadata.is_empty())
        || !ipc_field_type_matches(expected.field_type, actual)
    {
        return Err(AuraError::InvalidValue("shadow grouped ipc scalar field"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn preflight_grouped_record_batch(
    schema: &SchemaDescriptor,
    record_batch: arrow::ipc::RecordBatch<'_>,
    body: &[u8],
    total_events: &mut usize,
    total_children: &mut usize,
    variable_bytes: &mut usize,
    limits: ShadowGroupedProtocolLimits,
) -> Result<()> {
    let rows = usize::try_from(record_batch.length())
        .map_err(|_| AuraError::InvalidValue("shadow grouped ipc event count"))?;
    *total_events = total_events
        .checked_add(rows)
        .filter(|count| *count <= limits.events.max_events)
        .ok_or(AuraError::InvalidValue("shadow grouped ipc event count"))?;
    let nodes = record_batch
        .nodes()
        .ok_or(AuraError::InvalidValue("shadow grouped ipc field nodes"))?;
    let buffers = record_batch
        .buffers()
        .ok_or(AuraError::InvalidValue("shadow grouped ipc buffers"))?;
    if record_batch
        .variadicBufferCounts()
        .is_some_and(|counts| !counts.is_empty())
    {
        return Err(AuraError::InvalidValue("shadow grouped ipc flat layout"));
    }
    let (event_fields, repeated_fields) = scoped_fields(schema);
    let expected_nodes = event_fields
        .len()
        .checked_add(2)
        .and_then(|count| count.checked_add(repeated_fields.len()))
        .ok_or(AuraError::InvalidValue("shadow grouped ipc node count"))?;
    let scalar_buffers = |fields: &[&FieldDescriptor]| -> Result<usize> {
        fields.iter().try_fold(0usize, |count, field| {
            count
                .checked_add(
                    if matches!(field.field_type, FieldType::Utf8 | FieldType::DecimalText) {
                        3
                    } else {
                        2
                    },
                )
                .ok_or(AuraError::InvalidValue("shadow grouped ipc buffer count"))
        })
    };
    let expected_buffers = scalar_buffers(&event_fields)?
        .checked_add(3)
        .and_then(|count| count.checked_add(scalar_buffers(&repeated_fields).ok()?))
        .ok_or(AuraError::InvalidValue("shadow grouped ipc buffer count"))?;
    if nodes.len() != expected_nodes || buffers.len() != expected_buffers {
        return Err(AuraError::InvalidValue("shadow grouped ipc layout count"));
    }
    let mut node_index = 0usize;
    let mut buffer_index = 0usize;
    let mut previous_buffer_end = 0usize;
    for field in &event_fields {
        preflight_scalar_node(
            field,
            rows,
            0,
            rows,
            nodes.get(node_index),
            body,
            buffers,
            &mut buffer_index,
            &mut previous_buffer_end,
            variable_bytes,
            limits.events.max_variable_value_bytes,
        )?;
        node_index += 1;
    }

    let list_node = nodes.get(node_index);
    node_index += 1;
    if list_node.length() != record_batch.length() || list_node.null_count() != 0 {
        return Err(AuraError::InvalidValue("shadow grouped ipc list node"));
    }
    let validity = ipc_buffer(body, buffers.get(buffer_index), &mut previous_buffer_end)?;
    buffer_index += 1;
    validate_ipc_validity(validity, rows, 0)?;
    let offsets = ipc_buffer(body, buffers.get(buffer_index), &mut previous_buffer_end)?;
    buffer_index += 1;
    let (base, end) = validate_list_offsets(offsets, rows)?;

    let struct_node = nodes.get(node_index);
    node_index += 1;
    let struct_rows = usize::try_from(struct_node.length())
        .map_err(|_| AuraError::InvalidValue("shadow grouped ipc struct length"))?;
    if struct_node.null_count() != 0 || end > struct_rows {
        return Err(AuraError::InvalidValue("shadow grouped ipc struct node"));
    }
    let struct_validity = ipc_buffer(body, buffers.get(buffer_index), &mut previous_buffer_end)?;
    buffer_index += 1;
    validate_ipc_validity(struct_validity, struct_rows, 0)?;
    let relevant_children = end - base;
    *total_children = total_children
        .checked_add(relevant_children)
        .filter(|count| *count <= limits.events.max_children)
        .ok_or(AuraError::InvalidValue("shadow grouped ipc child count"))?;

    for field in &repeated_fields {
        preflight_scalar_node(
            field,
            struct_rows,
            base,
            end,
            nodes.get(node_index),
            body,
            buffers,
            &mut buffer_index,
            &mut previous_buffer_end,
            variable_bytes,
            limits.events.max_variable_value_bytes,
        )?;
        node_index += 1;
    }
    if node_index != nodes.len() || buffer_index != buffers.len() {
        return Err(AuraError::InvalidValue("shadow grouped ipc layout count"));
    }
    let expected_body_len = align_64(previous_buffer_end)?;
    if body.len() != expected_body_len || body[previous_buffer_end..].iter().any(|byte| *byte != 0)
    {
        return Err(AuraError::InvalidValue("shadow grouped ipc body padding"));
    }
    preflight_grouped_totals(
        &event_fields,
        &repeated_fields,
        *total_events,
        *total_children,
        *variable_bytes,
        limits.events,
    )
}

#[allow(clippy::too_many_arguments)]
fn preflight_scalar_node(
    field: &FieldDescriptor,
    rows: usize,
    semantic_start: usize,
    semantic_end: usize,
    node: &arrow::ipc::FieldNode,
    body: &[u8],
    buffers: flatbuffers::Vector<'_, arrow::ipc::Buffer>,
    buffer_index: &mut usize,
    previous_buffer_end: &mut usize,
    variable_bytes: &mut usize,
    value_limit: usize,
) -> Result<()> {
    if semantic_start > semantic_end || semantic_end > rows {
        return Err(AuraError::InvalidValue("shadow grouped ipc scalar range"));
    }
    let row_count =
        i64::try_from(rows).map_err(|_| AuraError::InvalidValue("shadow grouped ipc row count"))?;
    if node.length() != row_count
        || node.null_count() < 0
        || node.null_count() > row_count
        || (!field.nullable && node.null_count() != 0)
    {
        return Err(AuraError::InvalidValue("shadow grouped ipc scalar node"));
    }
    let validity = ipc_buffer(body, buffers.get(*buffer_index), previous_buffer_end)?;
    *buffer_index += 1;
    validate_ipc_validity(validity, rows, node.null_count() as usize)?;
    if matches!(field.field_type, FieldType::Utf8 | FieldType::DecimalText) {
        let offsets = ipc_buffer(body, buffers.get(*buffer_index), previous_buffer_end)?;
        let data = ipc_buffer(body, buffers.get(*buffer_index + 1), previous_buffer_end)?;
        *buffer_index += 2;
        validate_grouped_ipc_utf8_structure(offsets, data, rows)?;
        validate_logical_variable_values(
            field,
            offsets,
            data,
            validity,
            semantic_start,
            semantic_end,
            value_limit,
            variable_bytes,
        )?;
    } else {
        let values = ipc_buffer(body, buffers.get(*buffer_index), previous_buffer_end)?;
        *buffer_index += 1;
        let expected = rows
            .checked_mul(fixed_width(field.field_type))
            .ok_or(AuraError::InvalidValue("shadow grouped ipc value length"))?;
        if values.len() != expected {
            return Err(AuraError::InvalidValue("shadow grouped ipc value length"));
        }
        if (field.role == FieldRole::Boolean || field.role == FieldRole::Side)
            && (field.field_type != FieldType::U8
                || (semantic_start..semantic_end).any(|row| {
                    is_raw_present(validity, row) && values.get(row).is_some_and(|value| *value > 1)
                }))
        {
            return Err(AuraError::InvalidValue(
                "shadow grouped ipc boolean or side value",
            ));
        }
    }
    Ok(())
}

fn validate_list_offsets(offsets: &[u8], rows: usize) -> Result<(usize, usize)> {
    // Arrow IPC omits the otherwise-single zero offset buffer for an empty array.
    // The materialized ListArray still exposes the canonical `[0]` offsets.
    if rows == 0 && offsets.is_empty() {
        return Ok((0, 0));
    }
    let expected = rows
        .checked_add(1)
        .and_then(|count| count.checked_mul(4))
        .ok_or(AuraError::InvalidValue("shadow grouped list offsets"))?;
    if offsets.len() != expected {
        return Err(AuraError::InvalidValue("shadow grouped list offsets"));
    }
    let mut base = None;
    let mut previous = 0usize;
    for (index, bytes) in offsets.chunks_exact(4).enumerate() {
        let raw: [u8; 4] = bytes
            .try_into()
            .map_err(|_| AuraError::InvalidValue("shadow grouped list offsets"))?;
        let offset = usize::try_from(i32::from_le_bytes(raw))
            .map_err(|_| AuraError::InvalidValue("shadow grouped list offsets"))?;
        if index > 0 && offset < previous {
            return Err(AuraError::InvalidValue("shadow grouped list offsets"));
        }
        base.get_or_insert(offset);
        previous = offset;
    }
    Ok((base.unwrap_or(0), previous))
}

fn validate_grouped_ipc_utf8_structure(offsets: &[u8], data: &[u8], rows: usize) -> Result<()> {
    if rows == 0 && offsets.is_empty() {
        if data.is_empty() {
            return Ok(());
        }
        return Err(AuraError::InvalidValue("shadow grouped ipc utf8 data"));
    }
    let expected = rows
        .checked_add(1)
        .and_then(|count| count.checked_mul(4))
        .ok_or(AuraError::InvalidValue("shadow grouped ipc offset length"))?;
    if offsets.len() != expected {
        return Err(AuraError::InvalidValue("shadow grouped ipc offset length"));
    }
    let text = std::str::from_utf8(data)
        .map_err(|_| AuraError::InvalidValue("shadow grouped ipc utf8"))?;
    let mut previous = 0usize;
    for index in 0..=rows {
        let offset = ipc_i32_offset(offsets, index)?;
        if offset < previous || offset > data.len() || !text.is_char_boundary(offset) {
            return Err(AuraError::InvalidValue("shadow grouped ipc offset"));
        }
        previous = offset;
    }
    if previous != data.len() {
        return Err(AuraError::InvalidValue("shadow grouped ipc utf8"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_logical_variable_values(
    field: &FieldDescriptor,
    offsets: &[u8],
    data: &[u8],
    validity: &[u8],
    semantic_start: usize,
    semantic_end: usize,
    value_limit: usize,
    variable_bytes: &mut usize,
) -> Result<()> {
    for row in semantic_start..semantic_end {
        if !is_raw_present(validity, row) {
            continue;
        }
        let start = ipc_i32_offset(offsets, row)?;
        let end = ipc_i32_offset(offsets, row + 1)?;
        let length = end
            .checked_sub(start)
            .filter(|length| *length <= value_limit && *length <= MAX_V3_VARIABLE_VALUE_BYTES)
            .ok_or(AuraError::InvalidValue(
                "shadow grouped variable value length",
            ))?;
        *variable_bytes = variable_bytes
            .checked_add(length)
            .ok_or(AuraError::InvalidValue(
                "shadow grouped variable data length",
            ))?;
        if field.field_type == FieldType::DecimalText {
            let value = std::str::from_utf8(
                data.get(start..end)
                    .ok_or(AuraError::InvalidValue("shadow grouped decimal offset"))?,
            )
            .map_err(|_| AuraError::InvalidValue("shadow grouped decimal utf8"))?;
            validate_decimal_text_v1(value)?;
        }
    }
    Ok(())
}

fn ipc_i32_offset(offsets: &[u8], index: usize) -> Result<usize> {
    let start = index
        .checked_mul(4)
        .ok_or(AuraError::InvalidValue("shadow grouped ipc offset"))?;
    let end = start
        .checked_add(4)
        .ok_or(AuraError::InvalidValue("shadow grouped ipc offset"))?;
    let raw: [u8; 4] = offsets
        .get(start..end)
        .ok_or(AuraError::InvalidValue("shadow grouped ipc offset"))?
        .try_into()
        .map_err(|_| AuraError::InvalidValue("shadow grouped ipc offset"))?;
    usize::try_from(i32::from_le_bytes(raw))
        .map_err(|_| AuraError::InvalidValue("shadow grouped ipc offset"))
}

fn is_raw_present(validity: &[u8], row: usize) -> bool {
    validity.is_empty() || validity[row / 8] & (1 << (row % 8)) != 0
}

fn preflight_grouped_totals(
    event_fields: &[&FieldDescriptor],
    repeated_fields: &[&FieldDescriptor],
    events: usize,
    children: usize,
    variable_bytes: usize,
    limits: V3EventLimits,
) -> Result<()> {
    let values = events
        .checked_mul(event_fields.len())
        .and_then(|count| {
            children
                .checked_mul(repeated_fields.len())
                .and_then(|children| count.checked_add(children))
        })
        .filter(|count| *count <= limits.max_values)
        .ok_or(AuraError::InvalidValue("shadow grouped value count"))?;
    let _ = values;
    let mut block_len = 76usize
        .checked_add(
            events
                .checked_add(1)
                .and_then(|count| count.checked_mul(4))
                .ok_or(AuraError::InvalidValue("shadow grouped block length"))?,
        )
        .ok_or(AuraError::InvalidValue("shadow grouped block length"))?;
    for (fields, rows) in [(event_fields, events), (repeated_fields, children)] {
        for field in fields {
            block_len = block_len
                .checked_add(20)
                .and_then(|length| {
                    length.checked_add(if field.nullable {
                        rows.checked_add(7)? / 8
                    } else {
                        0
                    })
                })
                .ok_or(AuraError::InvalidValue("shadow grouped block length"))?;
            if matches!(field.field_type, FieldType::Utf8 | FieldType::DecimalText) {
                block_len = block_len
                    .checked_add(
                        rows.checked_add(1)
                            .and_then(|count| count.checked_mul(4))
                            .ok_or(AuraError::InvalidValue("shadow grouped block length"))?,
                    )
                    .ok_or(AuraError::InvalidValue("shadow grouped block length"))?;
            } else {
                block_len = block_len
                    .checked_add(
                        rows.checked_mul(fixed_width(field.field_type))
                            .ok_or(AuraError::InvalidValue("shadow grouped block length"))?,
                    )
                    .ok_or(AuraError::InvalidValue("shadow grouped block length"))?;
            }
        }
    }
    block_len = block_len
        .checked_add(variable_bytes)
        .ok_or(AuraError::InvalidValue("shadow grouped block length"))?;
    if block_len > limits.max_block_bytes || block_len > MAX_V3_EVENT_BLOCK_BYTES {
        return Err(AuraError::InvalidValue("shadow grouped block length"));
    }
    Ok(())
}
