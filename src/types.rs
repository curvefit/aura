use std::collections::{BTreeMap, BTreeSet};

use crate::schema::{AuraField, AuraSchema, AuraType};
use crate::{AuraError, Result};

/// Public Aura file profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Profile {
    /// Canonical normalized ingest file with generous logical values.
    Ingest = 0,
    /// Compact storage file compiled from ingest statistics.
    Aura0 = 1,
    /// Replay-optimized file compiled from ingest statistics.
    Aura1 = 2,
}

impl Profile {
    pub fn from_byte(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Ingest),
            1 => Ok(Self::Aura0),
            2 => Ok(Self::Aura1),
            other => Err(AuraError::InvalidProfile(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuraTypedValue {
    I64(i64),
    I128(i128),
    Opaque16([u8; 16]),
}

/// Public SDK scalar value.
#[derive(Debug, Clone, PartialEq)]
pub enum AuraValue {
    Bool(bool),
    U64(u64),
    I64(i64),
}

impl AuraValue {
    pub fn to_i64_for_type(&self, aura_type: AuraType) -> Result<i64> {
        match aura_type {
            AuraType::Bool => match self {
                Self::Bool(value) => Ok(i64::from(*value)),
                Self::U64(value) if *value <= 1 => Ok(*value as i64),
                Self::I64(value) if *value == 0 || *value == 1 => Ok(*value),
                _ => Err(AuraError::InvalidValue("bool value")),
            },
            AuraType::U8 | AuraType::EnumU8 => self.to_unsigned_range(u8::MAX as u64),
            AuraType::U16 => self.to_unsigned_range(u16::MAX as u64),
            AuraType::U32 | AuraType::FlagsU32 => self.to_unsigned_range(u32::MAX as u64),
            AuraType::U64 => self.to_unsigned_range(i64::MAX as u64),
            AuraType::I8 => self.to_signed_range(i8::MIN as i64, i8::MAX as i64),
            AuraType::I16 => self.to_signed_range(i16::MIN as i64, i16::MAX as i64),
            AuraType::I32 => self.to_signed_range(i32::MIN as i64, i32::MAX as i64),
            AuraType::I64
            | AuraType::TimestampNanos
            | AuraType::TimestampMicros
            | AuraType::I64Scaled { .. }
            | AuraType::PriceI64Scaled { .. } => self.to_signed_range(i64::MIN, i64::MAX),
            AuraType::F32 | AuraType::F64 | AuraType::Binary | AuraType::Utf8 => {
                Err(AuraError::InvalidValue("unsupported aura type"))
            }
        }
    }

    fn to_unsigned_range(&self, max: u64) -> Result<i64> {
        match self {
            Self::U64(value) if *value <= max => Ok(*value as i64),
            Self::I64(value) if *value >= 0 && (*value as u64) <= max => Ok(*value),
            Self::Bool(value) if max >= 1 => Ok(i64::from(*value)),
            _ => Err(AuraError::InvalidValue("unsigned value")),
        }
    }

    fn to_signed_range(&self, min: i64, max: i64) -> Result<i64> {
        match self {
            Self::I64(value) if *value >= min && *value <= max => Ok(*value),
            Self::U64(value) if *value <= max as u64 => Ok(*value as i64),
            Self::Bool(value) if min <= 0 && max >= 1 => Ok(i64::from(*value)),
            _ => Err(AuraError::InvalidValue("signed value")),
        }
    }

    pub fn from_i64_for_type(value: i64, aura_type: AuraType) -> Self {
        match aura_type {
            AuraType::Bool => Self::Bool(value != 0),
            AuraType::U8
            | AuraType::U16
            | AuraType::U32
            | AuraType::U64
            | AuraType::EnumU8
            | AuraType::FlagsU32 => Self::U64(value.max(0) as u64),
            _ => Self::I64(value),
        }
    }
}

impl From<i64> for AuraValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<u64> for AuraValue {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

impl From<bool> for AuraValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

/// Public SDK row batch.
#[derive(Debug, Clone, PartialEq)]
pub struct AuraRecordBatch {
    schema: AuraSchema,
    rows: Vec<Vec<AuraValue>>,
}

pub trait AuraBatch {
    fn into_record_batch(self) -> Result<AuraRecordBatch>;
}

impl AuraRecordBatch {
    pub fn new(schema: AuraSchema, rows: Vec<Vec<AuraValue>>) -> Result<Self> {
        for row in &rows {
            if row.len() != schema.field_count() {
                return Err(AuraError::InvalidValue("record field count"));
            }
        }
        Ok(Self { schema, rows })
    }

    pub fn from_i64_rows(schema: AuraSchema, rows: Vec<Vec<i64>>) -> Result<Self> {
        let values = rows
            .into_iter()
            .map(|row| row.into_iter().map(AuraValue::I64).collect())
            .collect();
        Self::new(schema, values)
    }

    pub fn schema(&self) -> &AuraSchema {
        &self.schema
    }

    pub fn rows(&self) -> &[Vec<AuraValue>] {
        &self.rows
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn to_i64_rows(&self) -> Result<Vec<Vec<i64>>> {
        self.rows
            .iter()
            .map(|row| {
                row.iter()
                    .zip(self.schema.fields())
                    .map(|(value, field)| value.to_i64_for_type(field.aura_type))
                    .collect()
            })
            .collect()
    }

    pub fn from_i64_decoded(schema: AuraSchema, rows: Vec<Vec<i64>>) -> Result<Self> {
        let fields = schema.fields().to_vec();
        let values = rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .zip(&fields)
                    .map(|(value, field)| AuraValue::from_i64_for_type(value, field.aura_type))
                    .collect()
            })
            .collect();
        Self::new(schema, values)
    }
}

impl AuraBatch for AuraRecordBatch {
    fn into_record_batch(self) -> Result<AuraRecordBatch> {
        Ok(self)
    }
}

/// Typed SDK column data.
#[derive(Debug, Clone, PartialEq)]
pub enum AuraColumn {
    Bool(Vec<bool>),
    U8(Vec<u8>),
    U16(Vec<u16>),
    U32(Vec<u32>),
    U64(Vec<u64>),
    I8(Vec<i8>),
    I16(Vec<i16>),
    I32(Vec<i32>),
    I64(Vec<i64>),
}

impl AuraColumn {
    pub fn len(&self) -> usize {
        match self {
            Self::Bool(values) => values.len(),
            Self::U8(values) => values.len(),
            Self::U16(values) => values.len(),
            Self::U32(values) => values.len(),
            Self::U64(values) => values.len(),
            Self::I8(values) => values.len(),
            Self::I16(values) => values.len(),
            Self::I32(values) => values.len(),
            Self::I64(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn matches_field(&self, field: &AuraField) -> bool {
        matches!(
            (self, field.aura_type),
            (Self::Bool(_), AuraType::Bool)
                | (Self::U8(_), AuraType::U8 | AuraType::EnumU8)
                | (Self::U16(_), AuraType::U16)
                | (Self::U32(_), AuraType::U32 | AuraType::FlagsU32)
                | (Self::U64(_), AuraType::U64)
                | (Self::I8(_), AuraType::I8)
                | (Self::I16(_), AuraType::I16)
                | (Self::I32(_), AuraType::I32)
                | (
                    Self::I64(_),
                    AuraType::I64
                        | AuraType::TimestampNanos
                        | AuraType::TimestampMicros
                        | AuraType::I64Scaled { .. }
                        | AuraType::PriceI64Scaled { .. }
                )
        )
    }

    fn value_at(&self, index: usize) -> AuraValue {
        match self {
            Self::Bool(values) => AuraValue::Bool(values[index]),
            Self::U8(values) => AuraValue::U64(u64::from(values[index])),
            Self::U16(values) => AuraValue::U64(u64::from(values[index])),
            Self::U32(values) => AuraValue::U64(u64::from(values[index])),
            Self::U64(values) => AuraValue::U64(values[index]),
            Self::I8(values) => AuraValue::I64(i64::from(values[index])),
            Self::I16(values) => AuraValue::I64(i64::from(values[index])),
            Self::I32(values) => AuraValue::I64(i64::from(values[index])),
            Self::I64(values) => AuraValue::I64(values[index]),
        }
    }

    pub(crate) fn from_i64_values_for_type(values: Vec<i64>, aura_type: AuraType) -> Result<Self> {
        match aura_type {
            AuraType::Bool => values
                .into_iter()
                .map(|value| match value {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(AuraError::InvalidValue("bool value")),
                })
                .collect::<Result<Vec<_>>>()
                .map(Self::Bool),
            AuraType::U8 | AuraType::EnumU8 => values
                .into_iter()
                .map(|value| u8::try_from(value).map_err(|_| AuraError::InvalidValue("u8 value")))
                .collect::<Result<Vec<_>>>()
                .map(Self::U8),
            AuraType::U16 => values
                .into_iter()
                .map(|value| u16::try_from(value).map_err(|_| AuraError::InvalidValue("u16 value")))
                .collect::<Result<Vec<_>>>()
                .map(Self::U16),
            AuraType::U32 | AuraType::FlagsU32 => values
                .into_iter()
                .map(|value| u32::try_from(value).map_err(|_| AuraError::InvalidValue("u32 value")))
                .collect::<Result<Vec<_>>>()
                .map(Self::U32),
            AuraType::U64 => values
                .into_iter()
                .map(|value| u64::try_from(value).map_err(|_| AuraError::InvalidValue("u64 value")))
                .collect::<Result<Vec<_>>>()
                .map(Self::U64),
            AuraType::I8 => values
                .into_iter()
                .map(|value| i8::try_from(value).map_err(|_| AuraError::InvalidValue("i8 value")))
                .collect::<Result<Vec<_>>>()
                .map(Self::I8),
            AuraType::I16 => values
                .into_iter()
                .map(|value| i16::try_from(value).map_err(|_| AuraError::InvalidValue("i16 value")))
                .collect::<Result<Vec<_>>>()
                .map(Self::I16),
            AuraType::I32 => values
                .into_iter()
                .map(|value| i32::try_from(value).map_err(|_| AuraError::InvalidValue("i32 value")))
                .collect::<Result<Vec<_>>>()
                .map(Self::I32),
            AuraType::I64
            | AuraType::TimestampNanos
            | AuraType::TimestampMicros
            | AuraType::I64Scaled { .. }
            | AuraType::PriceI64Scaled { .. } => Ok(Self::I64(values)),
            AuraType::F32 | AuraType::F64 | AuraType::Binary | AuraType::Utf8 => {
                Err(AuraError::InvalidValue("unsupported aura type"))
            }
        }
    }
}

/// Public SDK columnar batch.
#[derive(Debug, Clone, PartialEq)]
pub struct AuraColumnBatch {
    schema: AuraSchema,
    columns: Vec<AuraColumn>,
    row_count: usize,
}

impl AuraColumnBatch {
    pub fn builder(schema: AuraSchema) -> AuraColumnBatchBuilder {
        AuraColumnBatchBuilder {
            schema,
            columns: BTreeMap::new(),
        }
    }

    pub fn new(schema: AuraSchema, columns: Vec<(impl Into<String>, AuraColumn)>) -> Result<Self> {
        let mut builder = Self::builder(schema);
        for (name, column) in columns {
            builder = builder.column(name, column);
        }
        builder.build()
    }

    pub fn schema(&self) -> &AuraSchema {
        &self.schema
    }

    pub fn columns(&self) -> &[AuraColumn] {
        &self.columns
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn into_record_batch(self) -> Result<AuraRecordBatch> {
        let rows = (0..self.row_count)
            .map(|row_index| {
                self.columns
                    .iter()
                    .map(|column| column.value_at(row_index))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        AuraRecordBatch::new(self.schema, rows)
    }

    pub(crate) fn from_i64_columns(schema: AuraSchema, columns: Vec<Vec<i64>>) -> Result<Self> {
        if columns.len() != schema.field_count() {
            return Err(AuraError::InvalidValue("column count"));
        }
        let row_count = columns.first().map_or(0, Vec::len);
        let mut typed_columns = Vec::with_capacity(columns.len());
        for (values, field) in columns.into_iter().zip(schema.fields()) {
            if values.len() != row_count {
                return Err(AuraError::InvalidValue("column length"));
            }
            typed_columns.push(AuraColumn::from_i64_values_for_type(
                values,
                field.aura_type,
            )?);
        }
        Ok(Self {
            schema,
            columns: typed_columns,
            row_count,
        })
    }
}

impl AuraBatch for AuraColumnBatch {
    fn into_record_batch(self) -> Result<AuraRecordBatch> {
        AuraColumnBatch::into_record_batch(self)
    }
}

#[derive(Debug, Clone)]
pub struct AuraColumnBatchBuilder {
    schema: AuraSchema,
    columns: BTreeMap<String, AuraColumn>,
}

impl AuraColumnBatchBuilder {
    pub fn column(mut self, name: impl Into<String>, column: AuraColumn) -> Self {
        self.columns.insert(name.into(), column);
        self
    }

    pub fn bool(self, name: impl Into<String>, values: Vec<bool>) -> Self {
        self.column(name, AuraColumn::Bool(values))
    }

    pub fn u8(self, name: impl Into<String>, values: Vec<u8>) -> Self {
        self.column(name, AuraColumn::U8(values))
    }

    pub fn u16(self, name: impl Into<String>, values: Vec<u16>) -> Self {
        self.column(name, AuraColumn::U16(values))
    }

    pub fn u32(self, name: impl Into<String>, values: Vec<u32>) -> Self {
        self.column(name, AuraColumn::U32(values))
    }

    pub fn u64(self, name: impl Into<String>, values: Vec<u64>) -> Self {
        self.column(name, AuraColumn::U64(values))
    }

    pub fn i8(self, name: impl Into<String>, values: Vec<i8>) -> Self {
        self.column(name, AuraColumn::I8(values))
    }

    pub fn i16(self, name: impl Into<String>, values: Vec<i16>) -> Self {
        self.column(name, AuraColumn::I16(values))
    }

    pub fn i32(self, name: impl Into<String>, values: Vec<i32>) -> Self {
        self.column(name, AuraColumn::I32(values))
    }

    pub fn i64(self, name: impl Into<String>, values: Vec<i64>) -> Self {
        self.column(name, AuraColumn::I64(values))
    }

    pub fn build(self) -> Result<AuraColumnBatch> {
        let schema_names = self
            .schema
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<BTreeSet<_>>();
        for name in self.columns.keys() {
            if !schema_names.contains(name.as_str()) {
                return Err(AuraError::InvalidValue("extra column"));
            }
        }
        let mut row_count = None;
        let mut columns = Vec::with_capacity(self.schema.field_count());
        for field in self.schema.fields() {
            let column = self
                .columns
                .get(&field.name)
                .ok_or(AuraError::InvalidValue("missing column"))?;
            if !column.matches_field(field) {
                return Err(AuraError::InvalidValue("column type"));
            }
            match row_count {
                Some(expected) if expected != column.len() => {
                    return Err(AuraError::InvalidValue("column length"));
                }
                None => row_count = Some(column.len()),
                _ => {}
            }
            columns.push(column.clone());
        }
        Ok(AuraColumnBatch {
            schema: self.schema,
            columns,
            row_count: row_count.unwrap_or(0),
        })
    }
}

impl From<i64> for AuraTypedValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl AuraTypedValue {
    pub const fn observed_type(&self) -> &'static str {
        match self {
            Self::I64(_) => "i64",
            Self::I128(_) => "i128",
            Self::Opaque16(_) => "opaque16",
        }
    }

    pub fn observed_value_class(&self) -> &'static str {
        match self {
            Self::I64(value) if *value < 0 => "negative integer",
            Self::I64(_) => "integer",
            Self::I128(value) if *value < i64::MIN as i128 || *value > i64::MAX as i128 => {
                "wide integer"
            }
            Self::I128(_) => "integer",
            Self::Opaque16(_) => "fixed bytes",
        }
    }
}
