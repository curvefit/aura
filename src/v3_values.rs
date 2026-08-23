//! Standalone Aura v3 exact-value reference block.
//!
//! This module deliberately does not implement an `.aura`, `.aura0`, or
//! `.aura1` container. It is an uncompressed, schema-bound interchange block
//! used to define exact value and null semantics for the v3 dialect.

use sha2::{Digest, Sha256};

use crate::bytes::ByteReader;
use crate::schema::{
    encode_schema_descriptor, FieldScope, FieldType, SchemaDescriptor, SchemaEncodingVersion,
};
use crate::{AuraError, Result};

const MAGIC: &[u8; 8] = b"AURAV3VB";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 64;
const COLUMN_HEADER_BYTES: usize = 20;
const FLAG_VALIDITY: u8 = 1;
const FLAG_VARIABLE: u8 = 2;
const KNOWN_FLAGS: u8 = FLAG_VALIDITY | FLAG_VARIABLE;
const HASH_DOMAIN: &[u8] = b"aura-v3-canonical-exact-values-v1\0";
const SCHEMA_FINGERPRINT_DOMAIN: &[u8] = b"aura-v3-schema-fingerprint-v1\0";

/// Absolute implementation ceilings for the reference decoder.
pub const MAX_V3_VALUE_BLOCK_BYTES: usize = 1024 * 1024 * 1024;
pub const MAX_V3_VARIABLE_VALUE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_V3_VALUE_ROWS: usize = 16_777_216;
pub const MAX_V3_SCHEMA_DESCRIPTOR_BYTES: usize = crate::schema_json::MAX_SCHEMA_JSON_BYTES;

/// Caller-selectable ceilings. Values above the hard ceilings are clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3ValueLimits {
    pub max_block_bytes: usize,
    pub max_variable_value_bytes: usize,
    pub max_rows: usize,
}

impl V3ValueLimits {
    pub const HARD: Self = Self {
        max_block_bytes: MAX_V3_VALUE_BLOCK_BYTES,
        max_variable_value_bytes: MAX_V3_VARIABLE_VALUE_BYTES,
        max_rows: MAX_V3_VALUE_ROWS,
    };

    const fn effective(self) -> Self {
        Self {
            max_block_bytes: min_usize(self.max_block_bytes, MAX_V3_VALUE_BLOCK_BYTES),
            max_variable_value_bytes: min_usize(
                self.max_variable_value_bytes,
                MAX_V3_VARIABLE_VALUE_BYTES,
            ),
            max_rows: min_usize(self.max_rows, MAX_V3_VALUE_ROWS),
        }
    }
}

impl Default for V3ValueLimits {
    fn default() -> Self {
        Self::HARD
    }
}

const fn min_usize(left: usize, right: usize) -> usize {
    if left < right {
        left
    } else {
        right
    }
}

/// Contiguous variable-width values represented without per-row allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraV3VariableColumn {
    pub offsets: Vec<u32>,
    pub data: Vec<u8>,
}

/// Exact physical values for one stable schema slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuraV3ColumnValues {
    I8(Vec<i8>),
    U8(Vec<u8>),
    I16(Vec<i16>),
    U16(Vec<u16>),
    I32(Vec<i32>),
    U32(Vec<u32>),
    I64(Vec<i64>),
    U64(Vec<u64>),
    TimestampNs(Vec<i64>),
    I128(Vec<i128>),
    Opaque16(Vec<[u8; 16]>),
    TimestampMs(Vec<i64>),
    Utf8(AuraV3VariableColumn),
    DecimalText(AuraV3VariableColumn),
}

impl AuraV3ColumnValues {
    pub const fn field_type(&self) -> FieldType {
        match self {
            Self::I8(_) => FieldType::I8,
            Self::U8(_) => FieldType::U8,
            Self::I16(_) => FieldType::I16,
            Self::U16(_) => FieldType::U16,
            Self::I32(_) => FieldType::I32,
            Self::U32(_) => FieldType::U32,
            Self::I64(_) => FieldType::I64,
            Self::U64(_) => FieldType::U64,
            Self::TimestampNs(_) => FieldType::TimestampNs,
            Self::I128(_) => FieldType::I128,
            Self::Opaque16(_) => FieldType::Opaque16,
            Self::TimestampMs(_) => FieldType::TimestampMs,
            Self::Utf8(_) => FieldType::Utf8,
            Self::DecimalText(_) => FieldType::DecimalText,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::I8(values) => values.len(),
            Self::U8(values) => values.len(),
            Self::I16(values) => values.len(),
            Self::U16(values) => values.len(),
            Self::I32(values) => values.len(),
            Self::U32(values) => values.len(),
            Self::I64(values) | Self::TimestampNs(values) | Self::TimestampMs(values) => {
                values.len()
            }
            Self::U64(values) => values.len(),
            Self::I128(values) => values.len(),
            Self::Opaque16(values) => values.len(),
            Self::Utf8(values) | Self::DecimalText(values) => {
                values.offsets.len().saturating_sub(1)
            }
        }
    }

    const fn is_variable(&self) -> bool {
        matches!(self, Self::Utf8(_) | Self::DecimalText(_))
    }
}

/// One column in stable schema-slot order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraV3Column {
    pub slot: u16,
    /// Present-value bitmap for nullable fields; absent for nonnullable fields.
    pub validity: Option<Vec<u8>>,
    pub values: AuraV3ColumnValues,
}

impl AuraV3Column {
    pub fn value_ref(&self, row: usize) -> Result<Option<AuraV3ValueRef<'_>>> {
        if row >= self.values.len() {
            return Err(AuraError::InvalidValue("v3 value row index"));
        }
        if !present_at(self.validity.as_deref(), row)? {
            return Ok(None);
        }
        Ok(Some(match &self.values {
            AuraV3ColumnValues::I8(values) => AuraV3ValueRef::I8(values[row]),
            AuraV3ColumnValues::U8(values) => AuraV3ValueRef::U8(values[row]),
            AuraV3ColumnValues::I16(values) => AuraV3ValueRef::I16(values[row]),
            AuraV3ColumnValues::U16(values) => AuraV3ValueRef::U16(values[row]),
            AuraV3ColumnValues::I32(values) => AuraV3ValueRef::I32(values[row]),
            AuraV3ColumnValues::U32(values) => AuraV3ValueRef::U32(values[row]),
            AuraV3ColumnValues::I64(values) => AuraV3ValueRef::I64(values[row]),
            AuraV3ColumnValues::U64(values) => AuraV3ValueRef::U64(values[row]),
            AuraV3ColumnValues::TimestampNs(values) => AuraV3ValueRef::TimestampNs(values[row]),
            AuraV3ColumnValues::I128(values) => AuraV3ValueRef::I128(values[row]),
            AuraV3ColumnValues::Opaque16(values) => AuraV3ValueRef::Opaque16(&values[row]),
            AuraV3ColumnValues::TimestampMs(values) => AuraV3ValueRef::TimestampMs(values[row]),
            AuraV3ColumnValues::Utf8(values) => AuraV3ValueRef::Utf8(variable_str(values, row)?),
            AuraV3ColumnValues::DecimalText(values) => {
                AuraV3ValueRef::DecimalText(variable_str(values, row)?)
            }
        }))
    }
}

/// One schema-bound batch. Row order is exactly the supplied vector order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraV3Batch {
    pub schema_id: u32,
    pub row_count: u32,
    pub columns: Vec<AuraV3Column>,
}

/// Borrowed exact value returned by [`AuraV3Column::value_ref`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuraV3ValueRef<'a> {
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    TimestampNs(i64),
    I128(i128),
    Opaque16(&'a [u8; 16]),
    TimestampMs(i64),
    Utf8(&'a str),
    DecimalText(&'a str),
}

/// Validate the exact grammar while retaining the caller's original bytes.
pub fn validate_decimal_text_v1(value: &str) -> Result<()> {
    let trimmed = trim_decimal_outer_whitespace(value);
    if trimmed.is_empty() {
        return Err(AuraError::InvalidValue("decimal text v1"));
    }
    let body = trimmed
        .strip_prefix('+')
        .or_else(|| trimmed.strip_prefix('-'))
        .unwrap_or(trimmed);
    let mut digits = 0usize;
    let mut dots = 0usize;
    for byte in body.bytes() {
        if byte.is_ascii_digit() {
            digits += 1;
        } else if byte == b'.' {
            dots += 1;
            if dots > 1 {
                return Err(AuraError::InvalidValue("decimal text v1"));
            }
        } else {
            return Err(AuraError::InvalidValue("decimal text v1"));
        }
    }
    if digits == 0 {
        return Err(AuraError::InvalidValue("decimal text v1"));
    }
    Ok(())
}

fn trim_decimal_outer_whitespace(value: &str) -> &str {
    let mut start = 0usize;
    for (index, character) in value.char_indices() {
        if is_unicode_15_1_white_space(character) {
            start = index + character.len_utf8();
        } else {
            break;
        }
    }
    let mut end = value.len();
    for (index, character) in value.char_indices().rev() {
        if index < start {
            break;
        }
        if is_unicode_15_1_white_space(character) {
            end = index;
        } else {
            break;
        }
    }
    &value[start..end]
}

const fn is_unicode_15_1_white_space(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{0085}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// Compute the bounded canonical identity embedded in every V3 value block.
pub fn canonical_v3_schema_fingerprint(schema: &SchemaDescriptor) -> Result<[u8; 32]> {
    crate::schema::validate_v3_schema_identity(schema)?;
    crate::schema_json::preflight_schema_json_complexity(schema, MAX_V3_SCHEMA_DESCRIPTOR_BYTES)?;
    let schema_bytes = encode_schema_descriptor(schema)?;
    if schema_bytes.len() > MAX_V3_SCHEMA_DESCRIPTOR_BYTES
        || schema_bytes.get(4).copied() != Some(4)
    {
        return Err(AuraError::InvalidValue("v3 schema descriptor length"));
    }
    let mut hasher = Sha256::new();
    hasher.update(SCHEMA_FINGERPRINT_DOMAIN);
    hasher.update(
        u64::try_from(schema_bytes.len())
            .map_err(|_| AuraError::InvalidValue("v3 schema descriptor length"))?
            .to_le_bytes(),
    );
    hasher.update(schema_bytes);
    Ok(hasher.finalize().into())
}

/// Validate a schema-bound v3 batch under caller-selected (hard-clamped) limits.
pub fn validate_v3_batch(
    schema: &SchemaDescriptor,
    batch: &AuraV3Batch,
    limits: V3ValueLimits,
) -> Result<()> {
    let limits = limits.effective();
    validate_flat_v3_schema(schema)?;
    if batch.schema_id != schema.schema_id {
        return Err(AuraError::InvalidValue("v3 value schema id"));
    }
    let rows = usize::try_from(batch.row_count)
        .map_err(|_| AuraError::InvalidValue("v3 value row count"))?;
    if rows > limits.max_rows {
        return Err(AuraError::InvalidValue("v3 value row count"));
    }
    if batch.columns.len() != schema.fields.len() {
        return Err(AuraError::InvalidValue("v3 value column count"));
    }
    let validity_len = bitmap_len(rows)?;
    for (slot, (field, column)) in schema.fields.iter().zip(&batch.columns).enumerate() {
        if usize::from(column.slot) != slot || field.index != column.slot {
            return Err(AuraError::InvalidValue("v3 value column slot"));
        }
        if column.values.field_type() != field.field_type {
            return Err(AuraError::InvalidValue("v3 value column type"));
        }
        if column.values.len() != rows {
            return Err(AuraError::InvalidValue("v3 value column length"));
        }
        match (&column.validity, field.nullable) {
            (None, false) => {}
            (Some(bitmap), true) if bitmap.len() == validity_len => {
                validate_bitmap_padding(bitmap, rows)?;
            }
            (Some(_), false) => return Err(AuraError::InvalidValue("v3 value validity")),
            (None, true) => return Err(AuraError::InvalidValue("v3 value validity")),
            (Some(_), true) => return Err(AuraError::InvalidValue("v3 value validity length")),
        }
        validate_column_values(column, rows, limits.max_variable_value_bytes)?;
    }
    let length = encoded_len_value(batch, rows)?;
    if length > limits.max_block_bytes {
        return Err(AuraError::InvalidValue("v3 value block length"));
    }
    Ok(())
}

/// Encode the canonical standalone v3 exact-value block.
pub fn encode_v3_value_block(
    schema: &SchemaDescriptor,
    batch: &AuraV3Batch,
    limits: V3ValueLimits,
) -> Result<Vec<u8>> {
    validate_v3_batch(schema, batch, limits)?;
    let schema_fingerprint = canonical_v3_schema_fingerprint(schema)?;
    let rows = usize::try_from(batch.row_count)
        .map_err(|_| AuraError::InvalidValue("v3 value row count"))?;
    let total_len = encoded_len_value(batch, rows)?;
    let mut out = Vec::new();
    out.try_reserve_exact(total_len)
        .map_err(|_| AuraError::InvalidValue("v3 value allocation"))?;
    out.extend_from_slice(MAGIC);
    put_u16(&mut out, VERSION);
    put_u16(&mut out, 0);
    put_u32(&mut out, batch.schema_id);
    out.extend_from_slice(&schema_fingerprint);
    put_u32(&mut out, batch.row_count);
    put_u32(
        &mut out,
        u32::try_from(batch.columns.len())
            .map_err(|_| AuraError::InvalidValue("v3 value column count"))?,
    );
    put_u64(
        &mut out,
        u64::try_from(total_len).map_err(|_| AuraError::InvalidValue("v3 value block length"))?,
    );
    for column in &batch.columns {
        encode_column(column, rows, &mut out)?;
    }
    debug_assert_eq!(total_len, out.len());
    Ok(out)
}

/// Decode and canonically validate one standalone v3 exact-value block.
pub fn decode_v3_value_block(
    schema: &SchemaDescriptor,
    bytes: &[u8],
    limits: V3ValueLimits,
) -> Result<AuraV3Batch> {
    let limits = limits.effective();
    if bytes.len() > limits.max_block_bytes || bytes.len() > MAX_V3_VALUE_BLOCK_BYTES {
        return Err(AuraError::InvalidValue("v3 value block length"));
    }
    if bytes.len() < HEADER_BYTES {
        return Err(AuraError::UnexpectedEof);
    }
    validate_flat_v3_schema(schema)?;
    let mut reader = ByteReader::new(bytes);
    if reader.read_exact(MAGIC.len())? != MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "AURAV3VB",
        });
    }
    let version = reader.read_u16_le()?;
    if version != VERSION {
        return Err(AuraError::UnsupportedVersion(version));
    }
    if reader.read_u16_le()? != 0 {
        return Err(AuraError::InvalidValue("v3 value flags"));
    }
    let schema_id = reader.read_u32_le()?;
    if schema_id != schema.schema_id {
        return Err(AuraError::InvalidValue("v3 value schema id"));
    }
    let expected_fingerprint = canonical_v3_schema_fingerprint(schema)?;
    if reader.read_exact(expected_fingerprint.len())? != expected_fingerprint {
        return Err(AuraError::InvalidValue("v3 value schema fingerprint"));
    }
    let row_count = reader.read_u32_le()?;
    let rows =
        usize::try_from(row_count).map_err(|_| AuraError::InvalidValue("v3 value row count"))?;
    if rows > limits.max_rows {
        return Err(AuraError::InvalidValue("v3 value row count"));
    }
    let column_count = usize::try_from(reader.read_u32_le()?)
        .map_err(|_| AuraError::InvalidValue("v3 value column count"))?;
    if column_count != schema.fields.len() {
        return Err(AuraError::InvalidValue("v3 value column count"));
    }
    let declared_len = reader.read_u64_le()?;
    if declared_len
        != u64::try_from(bytes.len())
            .map_err(|_| AuraError::InvalidValue("v3 value block length"))?
        || declared_len < HEADER_BYTES as u64
    {
        return Err(AuraError::InvalidValue("v3 value block length"));
    }
    let minimum_headers = column_count
        .checked_mul(COLUMN_HEADER_BYTES)
        .and_then(|value| value.checked_add(HEADER_BYTES))
        .ok_or(AuraError::InvalidValue("v3 value block length"))?;
    if minimum_headers > bytes.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(column_count)
        .map_err(|_| AuraError::InvalidValue("v3 value allocation"))?;
    for field in &schema.fields {
        columns.push(decode_column(
            field.index,
            field.field_type,
            field.nullable,
            rows,
            &mut reader,
            limits,
        )?);
    }
    reader.finish()?;
    let batch = AuraV3Batch {
        schema_id,
        row_count,
        columns,
    };
    validate_v3_batch(schema, &batch, limits)?;
    Ok(batch)
}

/// Hash exact logical values, independent of physical null placeholders.
pub fn canonical_v3_batch_sha256(
    schema: &SchemaDescriptor,
    batch: &AuraV3Batch,
    limits: V3ValueLimits,
) -> Result<[u8; 32]> {
    let mut hasher = CanonicalV3RowHasher::new(schema, batch.row_count, limits)?;
    hasher.update_batch(schema, batch)?;
    hasher.finalize()
}

/// Incremental form of the canonical V3 logical-row hash.
///
/// The total row count is committed before any rows, so callers must know it
/// when constructing the hasher. Batches can then be supplied in file order
/// without retaining earlier decoded rows.
#[derive(Clone)]
pub struct CanonicalV3RowHasher {
    hasher: Sha256,
    schema_id: u32,
    schema_fingerprint: [u8; 32],
    column_count: usize,
    total_rows: u32,
    hashed_rows: u32,
    limits: V3ValueLimits,
}

pub type V3CanonicalRowHasher = CanonicalV3RowHasher;

impl core::fmt::Debug for CanonicalV3RowHasher {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CanonicalV3RowHasher")
            .field("schema_id", &self.schema_id)
            .field("column_count", &self.column_count)
            .field("total_rows", &self.total_rows)
            .field("hashed_rows", &self.hashed_rows)
            .finish_non_exhaustive()
    }
}

impl CanonicalV3RowHasher {
    pub fn new(schema: &SchemaDescriptor, total_rows: u32, limits: V3ValueLimits) -> Result<Self> {
        let limits = limits.effective();
        validate_flat_v3_schema(schema)?;
        if usize::try_from(total_rows).map_err(|_| AuraError::InvalidValue("v3 value row count"))?
            > limits.max_rows
        {
            return Err(AuraError::InvalidValue("v3 value row count"));
        }
        let schema_fingerprint = canonical_v3_schema_fingerprint(schema)?;
        let column_count = schema.fields.len();
        let mut hasher = Sha256::new();
        hasher.update(HASH_DOMAIN);
        hasher.update(schema.schema_id.to_le_bytes());
        hasher.update(schema_fingerprint);
        hasher.update(total_rows.to_le_bytes());
        hasher.update(
            u32::try_from(column_count)
                .map_err(|_| AuraError::InvalidValue("v3 value column count"))?
                .to_le_bytes(),
        );
        Ok(Self {
            hasher,
            schema_id: schema.schema_id,
            schema_fingerprint,
            column_count,
            total_rows,
            hashed_rows: 0,
            limits,
        })
    }

    pub const fn hashed_rows(&self) -> u32 {
        self.hashed_rows
    }

    pub fn update_batch(&mut self, schema: &SchemaDescriptor, batch: &AuraV3Batch) -> Result<()> {
        validate_v3_batch(schema, batch, self.limits)?;
        if schema.schema_id != self.schema_id
            || schema.fields.len() != self.column_count
            || canonical_v3_schema_fingerprint(schema)? != self.schema_fingerprint
        {
            return Err(AuraError::InvalidValue("v3 canonical hash schema"));
        }
        let next_rows = self
            .hashed_rows
            .checked_add(batch.row_count)
            .filter(|rows| *rows <= self.total_rows)
            .ok_or(AuraError::InvalidValue("v3 canonical hash row count"))?;
        for row in 0..usize::try_from(batch.row_count)
            .map_err(|_| AuraError::InvalidValue("v3 value row count"))?
        {
            for column in &batch.columns {
                self.hasher.update(column.slot.to_le_bytes());
                self.hasher.update([column.values.field_type() as u8]);
                let present = is_present(column.validity.as_deref(), row);
                self.hasher.update([u8::from(present)]);
                if present {
                    hash_present_value(&mut self.hasher, &column.values, row)?;
                }
            }
        }
        self.hashed_rows = next_rows;
        Ok(())
    }

    pub fn finalize(self) -> Result<[u8; 32]> {
        if self.hashed_rows != self.total_rows {
            return Err(AuraError::InvalidValue("v3 canonical hash row count"));
        }
        Ok(self.hasher.finalize().into())
    }
}

fn validate_flat_v3_schema(schema: &SchemaDescriptor) -> Result<()> {
    if schema.encoding_version != SchemaEncodingVersion::V3 {
        return Err(AuraError::InvalidValue("v3 value schema"));
    }
    crate::schema::validate_v3_schema_identity(schema)?;
    if !schema.groups.is_empty()
        || schema
            .fields
            .iter()
            .any(|field| field.scope == FieldScope::Repeated)
    {
        return Err(AuraError::InvalidValue("v3 value repeated schema"));
    }
    Ok(())
}

fn validate_column_values(column: &AuraV3Column, rows: usize, value_limit: usize) -> Result<()> {
    macro_rules! validate_fixed {
        ($values:expr, $zero:expr) => {{
            for (row, value) in $values.iter().enumerate() {
                if !is_present(column.validity.as_deref(), row) && *value != $zero {
                    return Err(AuraError::InvalidValue("v3 null placeholder"));
                }
            }
        }};
    }
    match &column.values {
        AuraV3ColumnValues::I8(values) => validate_fixed!(values, 0i8),
        AuraV3ColumnValues::U8(values) => validate_fixed!(values, 0u8),
        AuraV3ColumnValues::I16(values) => validate_fixed!(values, 0i16),
        AuraV3ColumnValues::U16(values) => validate_fixed!(values, 0u16),
        AuraV3ColumnValues::I32(values) => validate_fixed!(values, 0i32),
        AuraV3ColumnValues::U32(values) => validate_fixed!(values, 0u32),
        AuraV3ColumnValues::I64(values)
        | AuraV3ColumnValues::TimestampNs(values)
        | AuraV3ColumnValues::TimestampMs(values) => validate_fixed!(values, 0i64),
        AuraV3ColumnValues::U64(values) => validate_fixed!(values, 0u64),
        AuraV3ColumnValues::I128(values) => validate_fixed!(values, 0i128),
        AuraV3ColumnValues::Opaque16(values) => validate_fixed!(values, [0u8; 16]),
        AuraV3ColumnValues::Utf8(variable) => {
            validate_variable(
                variable,
                column.validity.as_deref(),
                rows,
                value_limit,
                false,
            )?;
        }
        AuraV3ColumnValues::DecimalText(variable) => {
            validate_variable(
                variable,
                column.validity.as_deref(),
                rows,
                value_limit,
                true,
            )?;
        }
    }
    Ok(())
}

fn validate_variable(
    variable: &AuraV3VariableColumn,
    validity: Option<&[u8]>,
    rows: usize,
    value_limit: usize,
    decimal: bool,
) -> Result<()> {
    let offset_count = rows
        .checked_add(1)
        .ok_or(AuraError::InvalidValue("v3 variable offsets"))?;
    if variable.offsets.len() != offset_count || variable.offsets.first() != Some(&0) {
        return Err(AuraError::InvalidValue("v3 variable offsets"));
    }
    let final_offset = usize::try_from(*variable.offsets.last().unwrap_or(&0))
        .map_err(|_| AuraError::InvalidValue("v3 variable offsets"))?;
    if final_offset != variable.data.len() {
        return Err(AuraError::InvalidValue("v3 variable offsets"));
    }
    for row in 0..rows {
        let start = variable.offsets[row] as usize;
        let end = variable.offsets[row + 1] as usize;
        if end < start || end > variable.data.len() {
            return Err(AuraError::InvalidValue("v3 variable offsets"));
        }
        if end - start > value_limit {
            return Err(AuraError::InvalidValue("v3 variable value length"));
        }
        if !is_present(validity, row) {
            if start != end {
                return Err(AuraError::InvalidValue("v3 null placeholder"));
            }
            continue;
        }
        let value = std::str::from_utf8(&variable.data[start..end])
            .map_err(|_| AuraError::InvalidValue("v3 utf8"))?;
        if decimal {
            validate_decimal_text_v1(value)?;
        }
    }
    Ok(())
}

fn bitmap_len(rows: usize) -> Result<usize> {
    rows.checked_add(7)
        .map(|value| value / 8)
        .ok_or(AuraError::InvalidValue("v3 value validity length"))
}

fn validate_bitmap_padding(bitmap: &[u8], rows: usize) -> Result<()> {
    let used = rows % 8;
    if used != 0 {
        let mask = !((1u8 << used) - 1);
        if bitmap.last().is_some_and(|byte| byte & mask != 0) {
            return Err(AuraError::InvalidValue("v3 value validity padding"));
        }
    }
    Ok(())
}

fn is_present(validity: Option<&[u8]>, row: usize) -> bool {
    validity.is_none_or(|bitmap| bitmap[row / 8] & (1 << (row % 8)) != 0)
}

fn present_at(validity: Option<&[u8]>, row: usize) -> Result<bool> {
    match validity {
        None => Ok(true),
        Some(bitmap) => bitmap
            .get(row / 8)
            .map(|byte| byte & (1 << (row % 8)) != 0)
            .ok_or(AuraError::InvalidValue("v3 value validity length")),
    }
}

fn variable_str(variable: &AuraV3VariableColumn, row: usize) -> Result<&str> {
    let start = *variable
        .offsets
        .get(row)
        .ok_or(AuraError::InvalidValue("v3 variable offsets"))? as usize;
    let end = *variable
        .offsets
        .get(
            row.checked_add(1)
                .ok_or(AuraError::InvalidValue("v3 variable offsets"))?,
        )
        .ok_or(AuraError::InvalidValue("v3 variable offsets"))? as usize;
    let bytes = variable
        .data
        .get(start..end)
        .ok_or(AuraError::InvalidValue("v3 variable offsets"))?;
    std::str::from_utf8(bytes).map_err(|_| AuraError::InvalidValue("v3 utf8"))
}

fn encoded_len_value(batch: &AuraV3Batch, rows: usize) -> Result<usize> {
    let mut total = HEADER_BYTES;
    for column in &batch.columns {
        total = total
            .checked_add(COLUMN_HEADER_BYTES)
            .and_then(|value| value.checked_add(column.validity.as_ref().map_or(0, Vec::len)))
            .ok_or(AuraError::InvalidValue("v3 value block length"))?;
        if column.values.is_variable() {
            let variable = match &column.values {
                AuraV3ColumnValues::Utf8(value) | AuraV3ColumnValues::DecimalText(value) => value,
                _ => unreachable!(),
            };
            total = total
                .checked_add(
                    rows.checked_add(1)
                        .and_then(|value| value.checked_mul(4))
                        .ok_or(AuraError::InvalidValue("v3 value block length"))?,
                )
                .and_then(|value| value.checked_add(variable.data.len()))
                .ok_or(AuraError::InvalidValue("v3 value block length"))?;
        } else {
            let width = fixed_width(column.values.field_type())
                .ok_or(AuraError::InvalidValue("v3 value column type"))?;
            total = total
                .checked_add(
                    rows.checked_mul(width)
                        .ok_or(AuraError::InvalidValue("v3 value block length"))?,
                )
                .ok_or(AuraError::InvalidValue("v3 value block length"))?;
        }
    }
    Ok(total)
}

fn fixed_width(field_type: FieldType) -> Option<usize> {
    match field_type {
        FieldType::I8 | FieldType::U8 => Some(1),
        FieldType::I16 | FieldType::U16 => Some(2),
        FieldType::I32 | FieldType::U32 => Some(4),
        FieldType::I64 | FieldType::U64 | FieldType::TimestampNs | FieldType::TimestampMs => {
            Some(8)
        }
        FieldType::I128 | FieldType::Opaque16 => Some(16),
        FieldType::Utf8 | FieldType::DecimalText => None,
    }
}

fn encode_column(column: &AuraV3Column, rows: usize, out: &mut Vec<u8>) -> Result<()> {
    let variable = column.values.is_variable();
    let validity_len = column.validity.as_ref().map_or(0, Vec::len);
    let (fixed_len, offsets_len, data_len) = if variable {
        let value = match &column.values {
            AuraV3ColumnValues::Utf8(value) | AuraV3ColumnValues::DecimalText(value) => value,
            _ => unreachable!(),
        };
        (
            0,
            value
                .offsets
                .len()
                .checked_mul(4)
                .ok_or(AuraError::InvalidValue("v3 variable offsets length"))?,
            value.data.len(),
        )
    } else {
        let width = fixed_width(column.values.field_type())
            .ok_or(AuraError::InvalidValue("v3 value column type"))?;
        (
            rows.checked_mul(width)
                .ok_or(AuraError::InvalidValue("v3 fixed value length"))?,
            0,
            0,
        )
    };
    put_u16(out, column.slot);
    out.push(column.values.field_type() as u8);
    out.push(
        (u8::from(column.validity.is_some()) * FLAG_VALIDITY)
            | (u8::from(variable) * FLAG_VARIABLE),
    );
    put_u32_len(out, validity_len, "v3 value validity length")?;
    put_u32_len(out, fixed_len, "v3 fixed value length")?;
    put_u32_len(out, offsets_len, "v3 variable offsets length")?;
    put_u32_len(out, data_len, "v3 variable data length")?;
    if let Some(validity) = &column.validity {
        out.extend_from_slice(validity);
    }
    encode_column_payload(&column.values, out);
    Ok(())
}

fn encode_column_payload(values: &AuraV3ColumnValues, out: &mut Vec<u8>) {
    macro_rules! extend_numeric {
        ($values:expr) => {
            for value in $values {
                out.extend_from_slice(&value.to_le_bytes());
            }
        };
    }
    match values {
        AuraV3ColumnValues::I8(values) => {
            out.extend(values.iter().map(|value| value.to_le_bytes()[0]));
        }
        AuraV3ColumnValues::U8(values) => out.extend_from_slice(values),
        AuraV3ColumnValues::I16(values) => extend_numeric!(values),
        AuraV3ColumnValues::U16(values) => extend_numeric!(values),
        AuraV3ColumnValues::I32(values) => extend_numeric!(values),
        AuraV3ColumnValues::U32(values) => extend_numeric!(values),
        AuraV3ColumnValues::I64(values)
        | AuraV3ColumnValues::TimestampNs(values)
        | AuraV3ColumnValues::TimestampMs(values) => extend_numeric!(values),
        AuraV3ColumnValues::U64(values) => extend_numeric!(values),
        AuraV3ColumnValues::I128(values) => extend_numeric!(values),
        AuraV3ColumnValues::Opaque16(values) => {
            for value in values {
                out.extend_from_slice(value);
            }
        }
        AuraV3ColumnValues::Utf8(value) | AuraV3ColumnValues::DecimalText(value) => {
            for offset in &value.offsets {
                put_u32(out, *offset);
            }
            out.extend_from_slice(&value.data);
        }
    }
}

fn decode_column(
    expected_slot: u16,
    expected_type: FieldType,
    nullable: bool,
    rows: usize,
    reader: &mut ByteReader<'_>,
    limits: V3ValueLimits,
) -> Result<AuraV3Column> {
    let slot = reader.read_u16_le()?;
    if slot != expected_slot {
        return Err(AuraError::InvalidValue("v3 value column slot"));
    }
    let field_type = FieldType::from_code(reader.read_u8()?)?;
    if field_type != expected_type {
        return Err(AuraError::InvalidValue("v3 value column type"));
    }
    let flags = reader.read_u8()?;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(AuraError::InvalidValue("v3 value column flags"));
    }
    let variable = fixed_width(field_type).is_none();
    if flags & FLAG_VARIABLE != u8::from(variable) * FLAG_VARIABLE
        || flags & FLAG_VALIDITY != u8::from(nullable) * FLAG_VALIDITY
    {
        return Err(AuraError::InvalidValue("v3 value column flags"));
    }
    let validity_len = reader.read_u32_le()? as usize;
    let fixed_len = reader.read_u32_le()? as usize;
    let offsets_len = reader.read_u32_le()? as usize;
    let data_len = reader.read_u32_le()? as usize;
    let expected_validity = if nullable { bitmap_len(rows)? } else { 0 };
    if validity_len != expected_validity {
        return Err(AuraError::InvalidValue("v3 value validity length"));
    }
    let expected_fixed = match fixed_width(field_type) {
        Some(width) => rows
            .checked_mul(width)
            .ok_or(AuraError::InvalidValue("v3 fixed value length"))?,
        None => 0,
    };
    let expected_offsets = if variable {
        rows.checked_add(1)
            .and_then(|value| value.checked_mul(4))
            .ok_or(AuraError::InvalidValue("v3 variable offsets length"))?
    } else {
        0
    };
    if fixed_len != expected_fixed
        || offsets_len != expected_offsets
        || (!variable && data_len != 0)
    {
        return Err(AuraError::InvalidValue("v3 value plane length"));
    }
    let plane_len = validity_len
        .checked_add(fixed_len)
        .and_then(|value| value.checked_add(offsets_len))
        .and_then(|value| value.checked_add(data_len))
        .ok_or(AuraError::InvalidValue("v3 value plane length"))?;
    if plane_len > reader.remaining() || data_len > limits.max_block_bytes {
        return Err(AuraError::UnexpectedEof);
    }
    let validity = if nullable {
        let bytes = reader.read_exact(validity_len)?;
        validate_bitmap_padding(bytes, rows)?;
        Some(copy_bytes(bytes)?)
    } else {
        None
    };
    let values = if variable {
        decode_variable(
            field_type,
            offsets_len,
            data_len,
            rows,
            reader,
            limits.max_variable_value_bytes,
        )?
    } else {
        decode_fixed(field_type, rows, reader.read_exact(fixed_len)?)?
    };
    Ok(AuraV3Column {
        slot,
        validity,
        values,
    })
}

fn decode_fixed(field_type: FieldType, rows: usize, bytes: &[u8]) -> Result<AuraV3ColumnValues> {
    macro_rules! decode_numbers {
        ($type:ty, $width:expr, $variant:ident) => {{
            let mut values = Vec::new();
            values
                .try_reserve_exact(rows)
                .map_err(|_| AuraError::InvalidValue("v3 value allocation"))?;
            for chunk in bytes.chunks_exact($width) {
                values.push(<$type>::from_le_bytes(
                    chunk
                        .try_into()
                        .map_err(|_| AuraError::InvalidValue("v3 fixed value length"))?,
                ));
            }
            AuraV3ColumnValues::$variant(values)
        }};
    }
    Ok(match field_type {
        FieldType::I8 => {
            let mut values = Vec::new();
            values
                .try_reserve_exact(rows)
                .map_err(|_| AuraError::InvalidValue("v3 value allocation"))?;
            values.extend(bytes.iter().map(|value| i8::from_le_bytes([*value])));
            AuraV3ColumnValues::I8(values)
        }
        FieldType::U8 => AuraV3ColumnValues::U8(copy_bytes(bytes)?),
        FieldType::I16 => decode_numbers!(i16, 2, I16),
        FieldType::U16 => decode_numbers!(u16, 2, U16),
        FieldType::I32 => decode_numbers!(i32, 4, I32),
        FieldType::U32 => decode_numbers!(u32, 4, U32),
        FieldType::I64 => decode_numbers!(i64, 8, I64),
        FieldType::U64 => decode_numbers!(u64, 8, U64),
        FieldType::TimestampNs => decode_numbers!(i64, 8, TimestampNs),
        FieldType::I128 => decode_numbers!(i128, 16, I128),
        FieldType::Opaque16 => {
            let mut values = Vec::new();
            values
                .try_reserve_exact(rows)
                .map_err(|_| AuraError::InvalidValue("v3 value allocation"))?;
            for chunk in bytes.chunks_exact(16) {
                values.push(
                    chunk
                        .try_into()
                        .map_err(|_| AuraError::InvalidValue("v3 fixed value length"))?,
                );
            }
            AuraV3ColumnValues::Opaque16(values)
        }
        FieldType::TimestampMs => decode_numbers!(i64, 8, TimestampMs),
        FieldType::Utf8 | FieldType::DecimalText => {
            return Err(AuraError::InvalidValue("v3 value column type"));
        }
    })
}

fn decode_variable(
    field_type: FieldType,
    offsets_len: usize,
    data_len: usize,
    rows: usize,
    reader: &mut ByteReader<'_>,
    value_limit: usize,
) -> Result<AuraV3ColumnValues> {
    let offset_bytes = reader.read_exact(offsets_len)?;
    let mut offsets = Vec::new();
    let offset_count = rows
        .checked_add(1)
        .ok_or(AuraError::InvalidValue("v3 variable offsets length"))?;
    offsets
        .try_reserve_exact(offset_count)
        .map_err(|_| AuraError::InvalidValue("v3 value allocation"))?;
    for chunk in offset_bytes.chunks_exact(4) {
        offsets.push(u32::from_le_bytes(
            chunk
                .try_into()
                .map_err(|_| AuraError::InvalidValue("v3 variable offsets"))?,
        ));
    }
    if offsets.first() != Some(&0)
        || offsets.last().copied()
            != Some(
                u32::try_from(data_len)
                    .map_err(|_| AuraError::InvalidValue("v3 variable data length"))?,
            )
    {
        return Err(AuraError::InvalidValue("v3 variable offsets"));
    }
    for pair in offsets.windows(2) {
        let start = pair[0] as usize;
        let end = pair[1] as usize;
        if end < start || end > data_len || end - start > value_limit {
            return Err(AuraError::InvalidValue("v3 variable offsets"));
        }
    }
    let data = copy_bytes(reader.read_exact(data_len)?)?;
    let variable = AuraV3VariableColumn { offsets, data };
    Ok(match field_type {
        FieldType::Utf8 => AuraV3ColumnValues::Utf8(variable),
        FieldType::DecimalText => AuraV3ColumnValues::DecimalText(variable),
        _ => return Err(AuraError::InvalidValue("v3 value column type")),
    })
}

fn hash_present_value(hasher: &mut Sha256, values: &AuraV3ColumnValues, row: usize) -> Result<()> {
    macro_rules! hash_numeric {
        ($values:expr) => {
            hasher.update($values[row].to_le_bytes())
        };
    }
    match values {
        AuraV3ColumnValues::I8(values) => hasher.update(values[row].to_le_bytes()),
        AuraV3ColumnValues::U8(values) => hasher.update([values[row]]),
        AuraV3ColumnValues::I16(values) => hash_numeric!(values),
        AuraV3ColumnValues::U16(values) => hash_numeric!(values),
        AuraV3ColumnValues::I32(values) => hash_numeric!(values),
        AuraV3ColumnValues::U32(values) => hash_numeric!(values),
        AuraV3ColumnValues::I64(values)
        | AuraV3ColumnValues::TimestampNs(values)
        | AuraV3ColumnValues::TimestampMs(values) => hash_numeric!(values),
        AuraV3ColumnValues::U64(values) => hash_numeric!(values),
        AuraV3ColumnValues::I128(values) => hash_numeric!(values),
        AuraV3ColumnValues::Opaque16(values) => hasher.update(values[row]),
        AuraV3ColumnValues::Utf8(variable) | AuraV3ColumnValues::DecimalText(variable) => {
            let start = variable.offsets[row] as usize;
            let end = variable.offsets[row + 1] as usize;
            let len = u32::try_from(end - start)
                .map_err(|_| AuraError::InvalidValue("v3 variable value length"))?;
            hasher.update(len.to_le_bytes());
            hasher.update(&variable.data[start..end]);
        }
    }
    Ok(())
}

fn copy_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(bytes.len())
        .map_err(|_| AuraError::InvalidValue("v3 value allocation"))?;
    out.extend_from_slice(bytes);
    Ok(out)
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32_len(out: &mut Vec<u8>, value: usize, name: &'static str) -> Result<()> {
    put_u32(
        out,
        u32::try_from(value).map_err(|_| AuraError::InvalidValue(name))?,
    );
    Ok(())
}
