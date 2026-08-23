//! Offline, provider-independent Arrow IPC boundary for Aura's reference V3 values.
//!
//! This module produces only a standalone exact-value block. It does not compile
//! or enable complete `.aura`, `.aura0`, or `.aura1` containers.

use std::io::{Cursor, Read};

use arrow::array::{
    Array, FixedSizeBinaryArray, Int16Array, Int32Array, Int64Array, Int8Array, StringArray,
    TimestampMillisecondArray, TimestampNanosecondArray, UInt16Array, UInt32Array, UInt64Array,
    UInt8Array,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::ipc::reader::StreamReader;
use arrow::ipc::{root_as_message, Endianness, MessageHeader, MetadataVersion};
use arrow::record_batch::RecordBatch;
use sha2::{Digest, Sha256};

use crate::schema::{FieldDescriptor, FieldRole, FieldType, SchemaDescriptor};
use crate::v3_values::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, encode_v3_value_block,
    validate_decimal_text_v1, validate_v3_batch, AuraV3Batch, AuraV3Column, AuraV3ColumnValues,
    AuraV3VariableColumn, V3ValueLimits, MAX_V3_VALUE_BLOCK_BYTES, MAX_V3_VALUE_ROWS,
    MAX_V3_VARIABLE_VALUE_BYTES,
};
use crate::{AuraError, Result};

pub const SHADOW_PROTOCOL: &str = "aura-logical-arrow-ipc-v1";
pub const SHADOW_ARTIFACT_KIND: &str = "standalone-aura-v3-value-block-v1";
pub const SHADOW_HANDSHAKE_SCHEMA: &str = "aura-shadow-handshake-v1";
pub const SHADOW_RESULT_SCHEMA: &str = "aura-shadow-encode-result-v1";
pub const SHADOW_VERIFY_RESULT_SCHEMA: &str = "aura-shadow-verify-result-v1";
pub const SHADOW_SCHEMA_FORMAT: &str = "aura-schema-json-v1";
pub const SHADOW_ARROW_PROTOCOL: &str = "arrow-ipc-stream";

/// Absolute input ceiling for one logical Arrow IPC stream (1 GiB).
pub const MAX_SHADOW_ARROW_IPC_BYTES: usize = 1024 * 1024 * 1024;
pub const DEFAULT_SHADOW_ARROW_IPC_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_SHADOW_VALUE_BLOCK_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_SHADOW_VALUE_ROWS: usize = 4 * 1024 * 1024;
pub const MAX_SHADOW_RECORD_BATCHES: usize = 65_536;
pub const DEFAULT_SHADOW_RECORD_BATCHES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowProtocolLimits {
    pub max_input_bytes: usize,
    pub max_record_batches: usize,
    pub values: V3ValueLimits,
}

impl ShadowProtocolLimits {
    pub const HARD: Self = Self {
        max_input_bytes: MAX_SHADOW_ARROW_IPC_BYTES,
        max_record_batches: MAX_SHADOW_RECORD_BATCHES,
        values: V3ValueLimits::HARD,
    };

    pub const DEFAULT: Self = Self {
        max_input_bytes: DEFAULT_SHADOW_ARROW_IPC_BYTES,
        max_record_batches: DEFAULT_SHADOW_RECORD_BATCHES,
        values: V3ValueLimits {
            max_block_bytes: DEFAULT_SHADOW_VALUE_BLOCK_BYTES,
            max_variable_value_bytes: MAX_V3_VARIABLE_VALUE_BYTES,
            max_rows: DEFAULT_SHADOW_VALUE_ROWS,
        },
    };

    fn effective(self) -> Self {
        Self {
            max_input_bytes: self.max_input_bytes.min(MAX_SHADOW_ARROW_IPC_BYTES),
            max_record_batches: self.max_record_batches.min(MAX_SHADOW_RECORD_BATCHES),
            values: V3ValueLimits {
                max_block_bytes: self.values.max_block_bytes.min(MAX_V3_VALUE_BLOCK_BYTES),
                max_variable_value_bytes: self
                    .values
                    .max_variable_value_bytes
                    .min(MAX_V3_VARIABLE_VALUE_BYTES),
                max_rows: self.values.max_rows.min(MAX_V3_VALUE_ROWS),
            },
        }
    }
}

impl Default for ShadowProtocolLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowEncodeResult {
    pub schema_fingerprint: [u8; 32],
    pub row_count: u32,
    pub logical_sha256: [u8; 32],
    pub block: Vec<u8>,
    pub block_sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildProvenance {
    pub git_commit: Option<&'static str>,
    pub dirty: Option<bool>,
    pub source: &'static str,
}

pub fn build_provenance() -> BuildProvenance {
    let commit = option_env!("AURA_EMBEDDED_GIT_COMMIT")
        .filter(|value| *value != "unavailable" && valid_embedded_commit(value));
    let dirty = match option_env!("AURA_EMBEDDED_GIT_DIRTY") {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    if commit.is_some() && dirty.is_some() {
        BuildProvenance {
            git_commit: commit,
            dirty,
            source: env!("AURA_EMBEDDED_PROVENANCE_SOURCE"),
        }
    } else {
        BuildProvenance {
            git_commit: None,
            dirty: None,
            source: "unavailable",
        }
    }
}

pub const fn arrow_rust_version() -> &'static str {
    env!("AURA_EMBEDDED_ARROW_VERSION")
}

pub fn cargo_lock_sha256() -> [u8; 32] {
    Sha256::digest(include_bytes!("../Cargo.lock")).into()
}

fn valid_embedded_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Read and convert one explicitly terminated Arrow IPC stream under bounded limits.
pub fn encode_shadow_arrow_ipc<R: Read>(
    schema: &SchemaDescriptor,
    mut input: R,
    limits: ShadowProtocolLimits,
) -> Result<ShadowEncodeResult> {
    let limits = limits.effective();
    let mut batch = empty_v3_batch(schema)?;
    validate_v3_batch(schema, &batch, limits.values)?;
    let ipc = read_bounded(&mut input, limits.max_input_bytes)?;
    preflight_ipc_stream(schema, &ipc, limits)?;
    batch = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decode_ipc_batches(schema, &ipc, batch, limits.values)
    }))
    .map_err(|_| AuraError::InvalidValue("shadow arrow ipc panic"))??;

    let schema_fingerprint = canonical_v3_schema_fingerprint(schema)?;
    let logical_sha256 = canonical_v3_batch_sha256(schema, &batch, limits.values)?;
    let block = encode_v3_value_block(schema, &batch, limits.values)?;
    let block_sha256 = Sha256::digest(&block).into();
    Ok(ShadowEncodeResult {
        schema_fingerprint,
        row_count: batch.row_count,
        logical_sha256,
        block,
        block_sha256,
    })
}

fn decode_ipc_batches(
    schema: &SchemaDescriptor,
    ipc: &[u8],
    mut batch: AuraV3Batch,
    limits: V3ValueLimits,
) -> Result<AuraV3Batch> {
    let cursor = Cursor::new(ipc);
    let mut reader = StreamReader::try_new(cursor, None)
        .map_err(|_| AuraError::InvalidValue("shadow arrow ipc"))?;
    validate_arrow_schema(schema, reader.schema().as_ref())?;
    for record_batch in &mut reader {
        let record_batch = record_batch.map_err(|_| AuraError::InvalidValue("shadow arrow ipc"))?;
        append_record_batch(schema, &mut batch, &record_batch, limits)?;
    }
    Ok(batch)
}

fn read_bounded(input: &mut impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve(limit.min(64 * 1024))
        .map_err(|_| AuraError::InvalidValue("shadow ipc allocation"))?;
    let mut scratch = [0u8; 16 * 1024];
    loop {
        let remaining = limit.saturating_sub(bytes.len());
        if remaining == 0 {
            let mut probe = [0u8; 1];
            match input.read(&mut probe) {
                Ok(0) => break,
                Ok(_) => return Err(AuraError::InvalidValue("shadow ipc input length")),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(AuraError::InvalidValue("shadow ipc input")),
            }
        }
        let request = remaining.min(scratch.len());
        let read = match input.read(&mut scratch[..request]) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(AuraError::InvalidValue("shadow ipc input")),
        };
        if read == 0 {
            break;
        }
        if bytes.capacity().saturating_sub(bytes.len()) < read {
            let doubled = bytes.capacity().max(64 * 1024).saturating_mul(2);
            let target = doubled.min(limit).max(bytes.len() + read);
            bytes
                .try_reserve(target - bytes.len())
                .map_err(|_| AuraError::InvalidValue("shadow ipc allocation"))?;
        }
        bytes.extend_from_slice(&scratch[..read]);
    }
    Ok(bytes)
}

/// Validate V5 framing and every flat batch buffer before Arrow can allocate or panic.
fn preflight_ipc_stream(
    schema: &SchemaDescriptor,
    bytes: &[u8],
    limits: ShadowProtocolLimits,
) -> Result<()> {
    let mut position = 0usize;
    let mut message_index = 0usize;
    let mut batch_count = 0usize;
    let mut total_rows = 0usize;
    let mut variable_data = Vec::new();
    variable_data
        .try_reserve_exact(schema.fields.len())
        .map_err(|_| AuraError::InvalidValue("shadow preflight allocation"))?;
    variable_data.resize(schema.fields.len(), 0usize);
    loop {
        if take_i32(bytes, &mut position)? != -1 {
            return Err(AuraError::InvalidValue("shadow ipc continuation marker"));
        }
        let metadata_len = take_i32(bytes, &mut position)?;
        if metadata_len == 0 {
            if message_index == 0 || position != bytes.len() {
                return Err(AuraError::InvalidValue("shadow ipc eos"));
            }
            return Ok(());
        }
        let metadata_len = usize::try_from(metadata_len)
            .map_err(|_| AuraError::InvalidValue("shadow ipc metadata length"))?;
        if metadata_len
            .checked_add(8)
            .is_none_or(|framed_len| framed_len % 64 != 0)
        {
            return Err(AuraError::InvalidValue("shadow ipc metadata alignment"));
        }
        let metadata_end = position
            .checked_add(metadata_len)
            .ok_or(AuraError::InvalidValue("shadow ipc metadata length"))?;
        let metadata = bytes
            .get(position..metadata_end)
            .ok_or(AuraError::UnexpectedEof)?;
        position = metadata_end;
        let message = root_as_message(metadata)
            .map_err(|_| AuraError::InvalidValue("shadow ipc metadata"))?;
        if message.version() != MetadataVersion::V5 {
            return Err(AuraError::InvalidValue("shadow ipc metadata version"));
        }
        if message
            .custom_metadata()
            .is_some_and(|metadata| !metadata.is_empty())
        {
            return Err(AuraError::InvalidValue("shadow ipc message metadata"));
        }
        let expected = if message_index == 0 {
            MessageHeader::Schema
        } else {
            MessageHeader::RecordBatch
        };
        if message.header_type() != expected {
            return Err(AuraError::InvalidValue("shadow ipc message type"));
        }
        let body_len = usize::try_from(message.bodyLength())
            .map_err(|_| AuraError::InvalidValue("shadow ipc body length"))?;
        if body_len % 64 != 0 {
            return Err(AuraError::InvalidValue("shadow ipc body alignment"));
        }
        let body_end = position
            .checked_add(body_len)
            .filter(|end| *end <= bytes.len())
            .ok_or(AuraError::UnexpectedEof)?;
        let body = &bytes[position..body_end];
        if message_index == 0 {
            let ipc_schema = message
                .header_as_schema()
                .ok_or(AuraError::InvalidValue("shadow ipc schema"))?;
            if ipc_schema.endianness() != Endianness::Little || !body.is_empty() {
                return Err(AuraError::InvalidValue("shadow ipc endianness"));
            }
            validate_ipc_flatbuffer_schema(schema, ipc_schema)?;
        } else {
            let record_batch = message
                .header_as_record_batch()
                .ok_or(AuraError::InvalidValue("shadow ipc record batch"))?;
            if record_batch.compression().is_some() {
                return Err(AuraError::InvalidValue("shadow ipc compression"));
            }
            batch_count = batch_count
                .checked_add(1)
                .filter(|count| *count <= limits.max_record_batches)
                .ok_or(AuraError::InvalidValue("shadow ipc batch count"))?;
            preflight_record_batch(
                schema,
                record_batch,
                body,
                &mut total_rows,
                &mut variable_data,
                limits.values,
            )?;
        }
        position = body_end;
        message_index = message_index
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("shadow ipc message count"))?;
    }
}

fn validate_ipc_flatbuffer_schema(
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
        return Err(AuraError::InvalidValue("shadow ipc schema metadata"));
    }
    let fields = actual
        .fields()
        .ok_or(AuraError::InvalidValue("shadow ipc schema fields"))?;
    if fields.len() != expected.fields.len() {
        return Err(AuraError::InvalidValue("shadow ipc schema fields"));
    }
    for (index, expected_field) in expected.fields.iter().enumerate() {
        let actual_field = fields.get(index);
        if actual_field.name() != Some(expected_field.name.as_str())
            || actual_field.nullable() != expected_field.nullable
            || actual_field.dictionary().is_some()
            || actual_field
                .children()
                .is_some_and(|children| !children.is_empty())
            || actual_field
                .custom_metadata()
                .is_some_and(|metadata| !metadata.is_empty())
            || !ipc_field_type_matches(expected_field.field_type, actual_field)
        {
            return Err(AuraError::InvalidValue("shadow ipc schema field"));
        }
    }
    Ok(())
}

fn ipc_field_type_matches(field_type: FieldType, field: arrow::ipc::Field<'_>) -> bool {
    match field_type {
        FieldType::I8
        | FieldType::U8
        | FieldType::I16
        | FieldType::U16
        | FieldType::I32
        | FieldType::U32
        | FieldType::I64
        | FieldType::U64 => {
            let (width, signed) = match field_type {
                FieldType::I8 => (8, true),
                FieldType::U8 => (8, false),
                FieldType::I16 => (16, true),
                FieldType::U16 => (16, false),
                FieldType::I32 => (32, true),
                FieldType::U32 => (32, false),
                FieldType::I64 => (64, true),
                FieldType::U64 => (64, false),
                _ => unreachable!(),
            };
            field.type_type() == arrow::ipc::Type::Int
                && field
                    .type_as_int()
                    .is_some_and(|value| value.bitWidth() == width && value.is_signed() == signed)
        }
        FieldType::TimestampNs | FieldType::TimestampMs => {
            let unit = if field_type == FieldType::TimestampNs {
                arrow::ipc::TimeUnit::NANOSECOND
            } else {
                arrow::ipc::TimeUnit::MILLISECOND
            };
            field.type_type() == arrow::ipc::Type::Timestamp
                && field
                    .type_as_timestamp()
                    .is_some_and(|value| value.unit() == unit && value.timezone().is_none())
        }
        FieldType::I128 | FieldType::Opaque16 => {
            field.type_type() == arrow::ipc::Type::FixedSizeBinary
                && field
                    .type_as_fixed_size_binary()
                    .is_some_and(|value| value.byteWidth() == 16)
        }
        FieldType::Utf8 | FieldType::DecimalText => {
            field.type_type() == arrow::ipc::Type::Utf8 && field.type_as_utf_8().is_some()
        }
    }
}

fn preflight_record_batch(
    schema: &SchemaDescriptor,
    record_batch: arrow::ipc::RecordBatch<'_>,
    body: &[u8],
    total_rows: &mut usize,
    variable_data: &mut [usize],
    limits: V3ValueLimits,
) -> Result<()> {
    let rows = usize::try_from(record_batch.length())
        .map_err(|_| AuraError::InvalidValue("shadow ipc row count"))?;
    *total_rows = total_rows
        .checked_add(rows)
        .filter(|value| *value <= limits.max_rows && *value <= MAX_V3_VALUE_ROWS)
        .ok_or(AuraError::InvalidValue("shadow ipc row count"))?;
    let nodes = record_batch
        .nodes()
        .ok_or(AuraError::InvalidValue("shadow ipc field nodes"))?;
    let buffers = record_batch
        .buffers()
        .ok_or(AuraError::InvalidValue("shadow ipc buffers"))?;
    if nodes.len() != schema.fields.len()
        || record_batch
            .variadicBufferCounts()
            .is_some_and(|counts| !counts.is_empty())
    {
        return Err(AuraError::InvalidValue("shadow ipc flat layout"));
    }
    let expected_buffers = schema.fields.iter().try_fold(0usize, |count, field| {
        count
            .checked_add(
                if matches!(field.field_type, FieldType::Utf8 | FieldType::DecimalText) {
                    3
                } else {
                    2
                },
            )
            .ok_or(AuraError::InvalidValue("shadow ipc buffer count"))
    })?;
    if buffers.len() != expected_buffers {
        return Err(AuraError::InvalidValue("shadow ipc buffer count"));
    }

    let mut buffer_index = 0usize;
    let mut previous_buffer_end = 0usize;
    for (field_index, field) in schema.fields.iter().enumerate() {
        let node = nodes.get(field_index);
        if node.length() != record_batch.length()
            || node.null_count() < 0
            || node.null_count() > record_batch.length()
            || (!field.nullable && node.null_count() != 0)
        {
            return Err(AuraError::InvalidValue("shadow ipc field node"));
        }
        let validity = ipc_buffer(body, buffers.get(buffer_index), &mut previous_buffer_end)?;
        buffer_index += 1;
        validate_ipc_validity(validity, rows, node.null_count() as usize)?;
        if matches!(field.field_type, FieldType::Utf8 | FieldType::DecimalText) {
            let offsets = ipc_buffer(body, buffers.get(buffer_index), &mut previous_buffer_end)?;
            let data = ipc_buffer(
                body,
                buffers.get(buffer_index + 1),
                &mut previous_buffer_end,
            )?;
            buffer_index += 2;
            validate_ipc_utf8(offsets, data, rows, limits.max_variable_value_bytes)?;
            variable_data[field_index] = variable_data[field_index]
                .checked_add(data.len())
                .filter(|length| *length <= u32::MAX as usize)
                .ok_or(AuraError::InvalidValue("shadow variable data length"))?;
        } else {
            let values = ipc_buffer(body, buffers.get(buffer_index), &mut previous_buffer_end)?;
            buffer_index += 1;
            let expected = rows
                .checked_mul(fixed_width(field.field_type))
                .ok_or(AuraError::InvalidValue("shadow ipc value length"))?;
            if values.len() != expected {
                return Err(AuraError::InvalidValue("shadow ipc value length"));
            }
        }
    }
    let expected_body_len = align_64(previous_buffer_end)?;
    if body.len() != expected_body_len || body[previous_buffer_end..].iter().any(|byte| *byte != 0)
    {
        return Err(AuraError::InvalidValue("shadow ipc body padding"));
    }
    preflight_total_block(schema, *total_rows, variable_data, limits)
}

fn ipc_buffer<'a>(
    body: &'a [u8],
    buffer: &arrow::ipc::Buffer,
    previous_end: &mut usize,
) -> Result<&'a [u8]> {
    let offset = usize::try_from(buffer.offset())
        .map_err(|_| AuraError::InvalidValue("shadow ipc buffer offset"))?;
    let length = usize::try_from(buffer.length())
        .map_err(|_| AuraError::InvalidValue("shadow ipc buffer length"))?;
    let end = offset
        .checked_add(length)
        .ok_or(AuraError::InvalidValue("shadow ipc buffer length"))?;
    let value = body
        .get(offset..end)
        .ok_or(AuraError::InvalidValue("shadow ipc buffer bounds"))?;
    let expected_offset = align_64(*previous_end)?;
    if offset != expected_offset || body[*previous_end..offset].iter().any(|byte| *byte != 0) {
        return Err(AuraError::InvalidValue("shadow ipc buffer alignment"));
    }
    *previous_end = end;
    Ok(value)
}

fn align_64(value: usize) -> Result<usize> {
    value
        .checked_add(63)
        .map(|value| value & !63)
        .ok_or(AuraError::InvalidValue("shadow ipc buffer alignment"))
}

fn validate_ipc_validity(validity: &[u8], rows: usize, null_count: usize) -> Result<()> {
    let bitmap_len = rows
        .checked_add(7)
        .ok_or(AuraError::InvalidValue("shadow ipc validity length"))?
        / 8;
    if validity.len() != bitmap_len && !(null_count == 0 && validity.is_empty()) {
        return Err(AuraError::InvalidValue("shadow ipc validity length"));
    }
    if !validity.is_empty() {
        let observed = (0..rows)
            .filter(|row| validity[*row / 8] & (1 << (*row % 8)) == 0)
            .count();
        if observed != null_count {
            return Err(AuraError::InvalidValue("shadow ipc null count"));
        }
    }
    Ok(())
}

fn validate_ipc_utf8(offsets: &[u8], data: &[u8], rows: usize, value_limit: usize) -> Result<()> {
    let expected_offsets = rows
        .checked_add(1)
        .and_then(|count| count.checked_mul(4))
        .ok_or(AuraError::InvalidValue("shadow ipc offset length"))?;
    if offsets.len() != expected_offsets {
        return Err(AuraError::InvalidValue("shadow ipc offset length"));
    }
    let mut previous = 0usize;
    let data_text =
        std::str::from_utf8(data).map_err(|_| AuraError::InvalidValue("shadow ipc utf8"))?;
    for (index, offset) in offsets.chunks_exact(4).enumerate() {
        let raw: [u8; 4] = offset
            .try_into()
            .map_err(|_| AuraError::InvalidValue("shadow ipc offset"))?;
        let offset = usize::try_from(i32::from_le_bytes(raw))
            .map_err(|_| AuraError::InvalidValue("shadow ipc offset"))?;
        if offset < previous
            || offset > data.len()
            || (index > 0
                && (offset - previous > value_limit
                    || offset - previous > MAX_V3_VARIABLE_VALUE_BYTES))
            || !data_text.is_char_boundary(offset)
        {
            return Err(AuraError::InvalidValue("shadow ipc offset"));
        }
        previous = offset;
    }
    if previous != data.len() {
        return Err(AuraError::InvalidValue("shadow ipc utf8"));
    }
    Ok(())
}

fn preflight_total_block(
    schema: &SchemaDescriptor,
    rows: usize,
    variable_data: &[usize],
    limits: V3ValueLimits,
) -> Result<()> {
    let mut block_len = 64usize;
    for (index, field) in schema.fields.iter().enumerate() {
        block_len = block_len
            .checked_add(20)
            .and_then(|value| {
                value.checked_add(if field.nullable {
                    rows.checked_add(7)? / 8
                } else {
                    0
                })
            })
            .ok_or(AuraError::InvalidValue("shadow block length"))?;
        if matches!(field.field_type, FieldType::Utf8 | FieldType::DecimalText) {
            let offsets_len = rows
                .checked_add(1)
                .and_then(|value| value.checked_mul(4))
                .ok_or(AuraError::InvalidValue("shadow block length"))?;
            block_len = block_len
                .checked_add(offsets_len)
                .and_then(|value| value.checked_add(variable_data[index]))
                .ok_or(AuraError::InvalidValue("shadow block length"))?;
        } else {
            let values_len = rows
                .checked_mul(fixed_width(field.field_type))
                .ok_or(AuraError::InvalidValue("shadow block length"))?;
            block_len = block_len
                .checked_add(values_len)
                .ok_or(AuraError::InvalidValue("shadow block length"))?;
        }
    }
    if block_len > limits.max_block_bytes || block_len > MAX_V3_VALUE_BLOCK_BYTES {
        return Err(AuraError::InvalidValue("shadow block length"));
    }
    Ok(())
}

fn take_i32(bytes: &[u8], position: &mut usize) -> Result<i32> {
    let end = position.checked_add(4).ok_or(AuraError::UnexpectedEof)?;
    let raw: [u8; 4] = bytes
        .get(*position..end)
        .ok_or(AuraError::UnexpectedEof)?
        .try_into()
        .map_err(|_| AuraError::UnexpectedEof)?;
    *position = end;
    Ok(i32::from_le_bytes(raw))
}

fn validate_arrow_schema(expected: &SchemaDescriptor, actual: &Schema) -> Result<()> {
    if !actual.metadata().is_empty() || actual.fields().len() != expected.fields.len() {
        return Err(AuraError::InvalidValue("shadow arrow schema"));
    }
    for (expected_field, actual_field) in expected.fields.iter().zip(actual.fields()) {
        validate_arrow_field(expected_field, actual_field)?;
    }
    Ok(())
}

fn validate_arrow_field(expected: &FieldDescriptor, actual: &Field) -> Result<()> {
    if !actual.metadata().is_empty()
        || actual.name() != &expected.name
        || actual.is_nullable() != expected.nullable
        || actual.data_type() != &arrow_type(expected.field_type)
    {
        return Err(AuraError::InvalidValue("shadow arrow field"));
    }
    Ok(())
}

fn arrow_type(field_type: FieldType) -> DataType {
    match field_type {
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
    }
}

fn empty_v3_batch(schema: &SchemaDescriptor) -> Result<AuraV3Batch> {
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(schema.fields.len())
        .map_err(|_| AuraError::InvalidValue("shadow column allocation"))?;
    for field in &schema.fields {
        let validity = field.nullable.then(Vec::new);
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
            FieldType::I128 => AuraV3ColumnValues::I128(Vec::new()),
            FieldType::Opaque16 => AuraV3ColumnValues::Opaque16(Vec::new()),
            FieldType::TimestampMs => AuraV3ColumnValues::TimestampMs(Vec::new()),
            FieldType::Utf8 | FieldType::DecimalText => {
                let mut offsets = Vec::new();
                offsets
                    .try_reserve_exact(1)
                    .map_err(|_| AuraError::InvalidValue("shadow offset allocation"))?;
                offsets.push(0);
                let variable = AuraV3VariableColumn {
                    offsets,
                    data: Vec::new(),
                };
                if field.field_type == FieldType::Utf8 {
                    AuraV3ColumnValues::Utf8(variable)
                } else {
                    AuraV3ColumnValues::DecimalText(variable)
                }
            }
        };
        columns.push(AuraV3Column {
            slot: field.index,
            validity,
            values,
        });
    }
    Ok(AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 0,
        columns,
    })
}

fn append_record_batch(
    schema: &SchemaDescriptor,
    output: &mut AuraV3Batch,
    input: &RecordBatch,
    limits: V3ValueLimits,
) -> Result<()> {
    validate_arrow_schema(schema, input.schema().as_ref())?;
    let old_rows = usize::try_from(output.row_count)
        .map_err(|_| AuraError::InvalidValue("shadow row count"))?;
    let new_rows = old_rows
        .checked_add(input.num_rows())
        .filter(|rows| *rows <= limits.max_rows && *rows <= MAX_V3_VALUE_ROWS)
        .ok_or(AuraError::InvalidValue("shadow row count"))?;
    preflight_batch(schema, output, input, new_rows, limits)?;

    for ((field, column), array) in schema
        .fields
        .iter()
        .zip(&mut output.columns)
        .zip(input.columns())
    {
        reserve_column(column, array.as_ref(), input.num_rows(), field.nullable)?;
        for row in 0..input.num_rows() {
            let present = !array.is_null(row);
            if !field.nullable && !present {
                return Err(AuraError::InvalidValue("shadow nonnullable null"));
            }
            if let Some(validity) = &mut column.validity {
                append_validity(validity, old_rows + row, present)?;
            }
            append_value(field, &mut column.values, array.as_ref(), row, present)?;
        }
    }
    output.row_count =
        u32::try_from(new_rows).map_err(|_| AuraError::InvalidValue("shadow row count"))?;
    Ok(())
}

fn preflight_batch(
    schema: &SchemaDescriptor,
    output: &AuraV3Batch,
    input: &RecordBatch,
    new_rows: usize,
    limits: V3ValueLimits,
) -> Result<()> {
    let mut block_len = 64usize;
    for ((field, column), array) in schema
        .fields
        .iter()
        .zip(&output.columns)
        .zip(input.columns())
    {
        block_len = block_len
            .checked_add(20)
            .and_then(|value| {
                value.checked_add(if field.nullable {
                    new_rows.checked_add(7)? / 8
                } else {
                    0
                })
            })
            .ok_or(AuraError::InvalidValue("shadow block length"))?;
        if matches!(field.field_type, FieldType::Utf8 | FieldType::DecimalText) {
            let current_data = match &column.values {
                AuraV3ColumnValues::Utf8(value) | AuraV3ColumnValues::DecimalText(value) => {
                    value.data.len()
                }
                _ => return Err(AuraError::InvalidValue("shadow column type")),
            };
            let strings = downcast::<StringArray>(array.as_ref())?;
            let mut added = 0usize;
            for row in 0..strings.len() {
                if strings.is_null(row) {
                    continue;
                }
                let value = strings.value(row);
                if value.len() > limits.max_variable_value_bytes
                    || value.len() > MAX_V3_VARIABLE_VALUE_BYTES
                {
                    return Err(AuraError::InvalidValue("shadow variable value length"));
                }
                if field.field_type == FieldType::DecimalText {
                    validate_decimal_text_v1(value)?;
                }
                added = added
                    .checked_add(value.len())
                    .ok_or(AuraError::InvalidValue("shadow variable data length"))?;
            }
            let data_len = current_data
                .checked_add(added)
                .filter(|value| *value <= u32::MAX as usize)
                .ok_or(AuraError::InvalidValue("shadow variable data length"))?;
            block_len = block_len
                .checked_add(
                    new_rows
                        .checked_add(1)
                        .and_then(|value| value.checked_mul(4))
                        .ok_or(AuraError::InvalidValue("shadow offset length"))?,
                )
                .and_then(|value| value.checked_add(data_len))
                .ok_or(AuraError::InvalidValue("shadow block length"))?;
        } else {
            block_len = block_len
                .checked_add(
                    new_rows
                        .checked_mul(fixed_width(field.field_type))
                        .ok_or(AuraError::InvalidValue("shadow fixed length"))?,
                )
                .ok_or(AuraError::InvalidValue("shadow block length"))?;
        }
        if field.role == FieldRole::Boolean {
            let values = downcast::<UInt8Array>(array.as_ref())?;
            for row in 0..values.len() {
                if !values.is_null(row) && values.value(row) > 1 {
                    return Err(AuraError::InvalidValue("shadow boolean value"));
                }
            }
        }
    }
    if block_len > limits.max_block_bytes || block_len > MAX_V3_VALUE_BLOCK_BYTES {
        return Err(AuraError::InvalidValue("shadow block length"));
    }
    Ok(())
}

fn reserve_column(
    column: &mut AuraV3Column,
    input: &dyn Array,
    rows: usize,
    nullable: bool,
) -> Result<()> {
    if nullable {
        let validity = column
            .validity
            .as_mut()
            .ok_or(AuraError::InvalidValue("shadow validity"))?;
        let final_rows = column_values_len(&column.values)
            .checked_add(rows)
            .ok_or(AuraError::InvalidValue("shadow validity length"))?;
        let final_bytes = final_rows
            .checked_add(7)
            .ok_or(AuraError::InvalidValue("shadow validity length"))?
            / 8;
        validity
            .try_reserve(final_bytes.saturating_sub(validity.len()))
            .map_err(|_| AuraError::InvalidValue("shadow validity allocation"))?;
    }
    macro_rules! reserve {
        ($values:expr) => {
            $values
                .try_reserve(rows)
                .map_err(|_| AuraError::InvalidValue("shadow value allocation"))?
        };
    }
    match &mut column.values {
        AuraV3ColumnValues::I8(values) => reserve!(values),
        AuraV3ColumnValues::U8(values) => reserve!(values),
        AuraV3ColumnValues::I16(values) => reserve!(values),
        AuraV3ColumnValues::U16(values) => reserve!(values),
        AuraV3ColumnValues::I32(values) => reserve!(values),
        AuraV3ColumnValues::U32(values) => reserve!(values),
        AuraV3ColumnValues::I64(values) => reserve!(values),
        AuraV3ColumnValues::U64(values) => reserve!(values),
        AuraV3ColumnValues::TimestampNs(values) => reserve!(values),
        AuraV3ColumnValues::I128(values) => reserve!(values),
        AuraV3ColumnValues::Opaque16(values) => reserve!(values),
        AuraV3ColumnValues::TimestampMs(values) => reserve!(values),
        AuraV3ColumnValues::Utf8(variable) | AuraV3ColumnValues::DecimalText(variable) => {
            variable
                .offsets
                .try_reserve(rows)
                .map_err(|_| AuraError::InvalidValue("shadow offset allocation"))?;
            let strings = downcast::<StringArray>(input)?;
            let added = (0..strings.len()).try_fold(0usize, |total, row| {
                total
                    .checked_add(if strings.is_null(row) {
                        0
                    } else {
                        strings.value(row).len()
                    })
                    .ok_or(AuraError::InvalidValue("shadow variable data length"))
            })?;
            variable
                .data
                .try_reserve(added)
                .map_err(|_| AuraError::InvalidValue("shadow variable allocation"))?;
        }
    }
    Ok(())
}

fn column_values_len(values: &AuraV3ColumnValues) -> usize {
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
        AuraV3ColumnValues::I128(values) => values.len(),
        AuraV3ColumnValues::Opaque16(values) => values.len(),
        AuraV3ColumnValues::TimestampMs(values) => values.len(),
        AuraV3ColumnValues::Utf8(values) | AuraV3ColumnValues::DecimalText(values) => {
            values.offsets.len().saturating_sub(1)
        }
    }
}

fn append_validity(bitmap: &mut Vec<u8>, row: usize, present: bool) -> Result<()> {
    let byte = row / 8;
    if byte == bitmap.len() {
        bitmap.push(0);
    } else if byte > bitmap.len() {
        return Err(AuraError::InvalidValue("shadow validity length"));
    }
    if present {
        bitmap[byte] |= 1 << (row % 8);
    }
    Ok(())
}

fn append_value(
    field: &FieldDescriptor,
    output: &mut AuraV3ColumnValues,
    input: &dyn Array,
    row: usize,
    present: bool,
) -> Result<()> {
    macro_rules! append_primitive {
        ($variant:ident, $array:ty, $zero:expr) => {{
            let array = downcast::<$array>(input)?;
            match output {
                AuraV3ColumnValues::$variant(values) => {
                    values.push(if present { array.value(row) } else { $zero });
                    Ok(())
                }
                _ => Err(AuraError::InvalidValue("shadow column type")),
            }
        }};
    }
    match field.field_type {
        FieldType::I8 => append_primitive!(I8, Int8Array, 0),
        FieldType::U8 => append_primitive!(U8, UInt8Array, 0),
        FieldType::I16 => append_primitive!(I16, Int16Array, 0),
        FieldType::U16 => append_primitive!(U16, UInt16Array, 0),
        FieldType::I32 => append_primitive!(I32, Int32Array, 0),
        FieldType::U32 => append_primitive!(U32, UInt32Array, 0),
        FieldType::I64 => append_primitive!(I64, Int64Array, 0),
        FieldType::U64 => append_primitive!(U64, UInt64Array, 0),
        FieldType::TimestampNs => {
            append_primitive!(TimestampNs, TimestampNanosecondArray, 0)
        }
        FieldType::TimestampMs => {
            append_primitive!(TimestampMs, TimestampMillisecondArray, 0)
        }
        FieldType::I128 => {
            let array = downcast::<FixedSizeBinaryArray>(input)?;
            let value = if present {
                i128::from_le_bytes(
                    array
                        .value(row)
                        .try_into()
                        .map_err(|_| AuraError::InvalidValue("shadow i128 width"))?,
                )
            } else {
                0
            };
            match output {
                AuraV3ColumnValues::I128(values) => {
                    values.push(value);
                    Ok(())
                }
                _ => Err(AuraError::InvalidValue("shadow column type")),
            }
        }
        FieldType::Opaque16 => {
            let array = downcast::<FixedSizeBinaryArray>(input)?;
            let value = if present {
                array
                    .value(row)
                    .try_into()
                    .map_err(|_| AuraError::InvalidValue("shadow opaque width"))?
            } else {
                [0; 16]
            };
            match output {
                AuraV3ColumnValues::Opaque16(values) => {
                    values.push(value);
                    Ok(())
                }
                _ => Err(AuraError::InvalidValue("shadow column type")),
            }
        }
        FieldType::Utf8 | FieldType::DecimalText => {
            let array = downcast::<StringArray>(input)?;
            let variable = match output {
                AuraV3ColumnValues::Utf8(value) | AuraV3ColumnValues::DecimalText(value) => value,
                _ => return Err(AuraError::InvalidValue("shadow column type")),
            };
            if present {
                let bytes = array.value(row).as_bytes();
                variable
                    .data
                    .try_reserve_exact(bytes.len())
                    .map_err(|_| AuraError::InvalidValue("shadow variable allocation"))?;
                variable.data.extend_from_slice(bytes);
            }
            variable.offsets.push(
                u32::try_from(variable.data.len())
                    .map_err(|_| AuraError::InvalidValue("shadow variable data length"))?,
            );
            Ok(())
        }
    }
}

fn downcast<T: Array + 'static>(array: &dyn Array) -> Result<&T> {
    array
        .as_any()
        .downcast_ref::<T>()
        .ok_or(AuraError::InvalidValue("shadow arrow array type"))
}

fn fixed_width(field_type: FieldType) -> usize {
    match field_type {
        FieldType::I8 | FieldType::U8 => 1,
        FieldType::I16 | FieldType::U16 => 2,
        FieldType::I32 | FieldType::U32 => 4,
        FieldType::I64 | FieldType::U64 | FieldType::TimestampNs | FieldType::TimestampMs => 8,
        FieldType::I128 | FieldType::Opaque16 => 16,
        FieldType::Utf8 | FieldType::DecimalText => 0,
    }
}
