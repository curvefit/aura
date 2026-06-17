use std::collections::BTreeSet;

use crate::records::{self, I64FileInput};
use crate::schema::{FieldType, SchemaDescriptor};
use crate::{AuraDiagnostic, AuraError, Profile, Result};

/// In-memory writer for positional i64 Aura ingest files.
///
/// This is the public ownership boundary for sealing `.aura` files and
/// compiling them to `.aura0`/`.aura1`. The existing record implementation
/// remains the compatibility layer behind this facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraI64Writer {
    schema: SchemaDescriptor,
    rows: Vec<Vec<i64>>,
    stream_id: u16,
    dictionary_id: u16,
    header_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuraTypedValue {
    I64(i64),
    I128(i128),
    Opaque16([u8; 16]),
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

/// Declared-layout writer for typed row values.
///
/// Phase 3 keeps the sealed body implementation i64-only. Explicit `i128` and
/// opaque 16-byte declarations are accepted at the API boundary, then rejected
/// with structured diagnostics before sealing until a lossless wide body exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraTypedWriter {
    schema: SchemaDescriptor,
    rows: Vec<Vec<AuraTypedValue>>,
    stream_id: u16,
    dictionary_id: u16,
    header_comment: Option<String>,
    internally_derived_slots: BTreeSet<u16>,
}

impl AuraI64Writer {
    pub fn new(schema: SchemaDescriptor) -> Self {
        Self {
            schema,
            rows: Vec::new(),
            stream_id: 0,
            dictionary_id: 0,
            header_comment: None,
        }
    }

    pub fn from_input(input: I64FileInput) -> Self {
        Self {
            schema: input.schema,
            rows: input.rows,
            stream_id: input.stream_id,
            dictionary_id: input.dictionary_id,
            header_comment: input.header_comment,
        }
    }

    pub fn with_stream(mut self, stream_id: u16, dictionary_id: u16) -> Self {
        self.stream_id = stream_id;
        self.dictionary_id = dictionary_id;
        self
    }

    pub fn with_header_comment(mut self, comment: impl Into<String>) -> Self {
        self.header_comment = Some(comment.into());
        self
    }

    pub fn push_row(&mut self, row: impl Into<Vec<i64>>) -> Result<&mut Self> {
        let row = row.into();
        if row.len() != self.schema.fields.len() {
            return Err(AuraError::InvalidValue("record field count"));
        }
        self.rows.push(row);
        Ok(self)
    }

    pub fn extend_rows<I, R>(&mut self, rows: I) -> Result<&mut Self>
    where
        I: IntoIterator<Item = R>,
        R: Into<Vec<i64>>,
    {
        for row in rows {
            self.push_row(row)?;
        }
        Ok(self)
    }

    pub fn schema(&self) -> &SchemaDescriptor {
        &self.schema
    }

    pub fn rows(&self) -> &[Vec<i64>] {
        &self.rows
    }

    pub fn into_input(self) -> I64FileInput {
        I64FileInput {
            schema: self.schema,
            rows: self.rows,
            stream_id: self.stream_id,
            dictionary_id: self.dictionary_id,
            header_comment: self.header_comment,
        }
    }

    pub fn finish(self) -> Result<Vec<u8>> {
        stamp_i64(self.into_input())
    }

    pub fn compile_profile(bytes: &[u8], target_profile: Profile) -> Result<Vec<u8>> {
        compile_i64(bytes, target_profile)
    }
}

impl AuraTypedWriter {
    pub fn new(schema: SchemaDescriptor) -> Self {
        Self {
            schema,
            rows: Vec::new(),
            stream_id: 0,
            dictionary_id: 0,
            header_comment: None,
            internally_derived_slots: BTreeSet::new(),
        }
    }

    pub fn with_stream(mut self, stream_id: u16, dictionary_id: u16) -> Self {
        self.stream_id = stream_id;
        self.dictionary_id = dictionary_id;
        self
    }

    pub fn with_header_comment(mut self, comment: impl Into<String>) -> Self {
        self.header_comment = Some(comment.into());
        self
    }

    pub fn mark_internal_derivation(&mut self, slot_index: u16) -> Result<&mut Self> {
        if usize::from(slot_index) >= self.schema.fields.len() {
            return Err(layout_diagnostic(
                None,
                Some(slot_index),
                "field",
                "internal_derivation",
                "slot out of range",
                "schema slot",
                None,
            ));
        }
        self.internally_derived_slots.insert(slot_index);
        Ok(self)
    }

    pub fn push_row(&mut self, row: impl Into<Vec<AuraTypedValue>>) -> Result<&mut Self> {
        let row = row.into();
        let row_index = self.rows.len();
        if row.len() != self.schema.fields.len() {
            return Err(layout_diagnostic(
                Some(row_index),
                None,
                "row",
                "row",
                "field count mismatch",
                "wrong slot count",
                None,
            ));
        }
        for (slot_index, value) in row.iter().enumerate() {
            let slot_index =
                u16::try_from(slot_index).map_err(|_| AuraError::InvalidValue("field index"))?;
            if self.internally_derived_slots.contains(&slot_index) {
                return Err(layout_diagnostic(
                    Some(row_index),
                    Some(slot_index),
                    self.schema.fields[usize::from(slot_index)]
                        .field_type
                        .name(),
                    value.observed_type(),
                    "derived slot source conflict",
                    value.observed_value_class(),
                    Some("remove supplied value or disable internal derivation"),
                ));
            }
            validate_typed_value(
                row_index,
                slot_index,
                self.schema.fields[usize::from(slot_index)].field_type,
                value,
            )?;
        }
        self.rows.push(row);
        Ok(self)
    }

    pub fn extend_rows<I, R>(&mut self, rows: I) -> Result<&mut Self>
    where
        I: IntoIterator<Item = R>,
        R: Into<Vec<AuraTypedValue>>,
    {
        for row in rows {
            self.push_row(row)?;
        }
        Ok(self)
    }

    pub fn schema(&self) -> &SchemaDescriptor {
        &self.schema
    }

    pub fn rows(&self) -> &[Vec<AuraTypedValue>] {
        &self.rows
    }

    pub fn finish(self) -> Result<Vec<u8>> {
        if let Some(field) = self
            .schema
            .fields
            .iter()
            .find(|field| matches!(field.field_type, FieldType::I128 | FieldType::Opaque16))
        {
            return Err(layout_diagnostic(
                None,
                Some(field.index),
                field.field_type.name(),
                field.field_type.name(),
                "unsupported profile",
                "wide field",
                Some("wait for lossless wide-field body support"),
            ));
        }

        let rows = self
            .rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|value| match value {
                        AuraTypedValue::I64(value) => Ok(value),
                        AuraTypedValue::I128(_) => Err(layout_diagnostic(
                            None,
                            None,
                            "i64",
                            "i128",
                            "unsupported profile",
                            "wide integer",
                            Some("declare i128 and wait for wide-field body support"),
                        )),
                        AuraTypedValue::Opaque16(_) => Err(layout_diagnostic(
                            None,
                            None,
                            "i64",
                            "opaque16",
                            "unsupported profile",
                            "fixed bytes",
                            Some("declare opaque16 and wait for wide-field body support"),
                        )),
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;

        encode_i64(I64FileInput {
            schema: self.schema,
            rows,
            stream_id: self.stream_id,
            dictionary_id: self.dictionary_id,
            header_comment: self.header_comment,
        })
    }
}

pub fn encode_i64(input: I64FileInput) -> Result<Vec<u8>> {
    validate_i64_input(&input.schema, &input.rows)?;
    records::encode_ingest_i64_file_inner(input)
}

pub fn stamp_i64(input: I64FileInput) -> Result<Vec<u8>> {
    encode_i64(input)
}

pub fn compile_i64(bytes: &[u8], target_profile: Profile) -> Result<Vec<u8>> {
    records::compile_i64_file_inner(bytes, target_profile)
}

pub fn restamp_i64(bytes: &[u8], schema: SchemaDescriptor) -> Result<Vec<u8>> {
    let decoded = crate::reader::decode_i64(bytes)?;
    validate_i64_input(&schema, &decoded.rows)?;
    stamp_i64(I64FileInput {
        schema,
        rows: decoded.rows,
        stream_id: decoded.header.stream_id,
        dictionary_id: decoded.header.dictionary_id,
        header_comment: if decoded.header.comment.is_empty() {
            None
        } else {
            Some(decoded.header.comment)
        },
    })
}

fn validate_typed_value(
    row_index: usize,
    slot_index: u16,
    declared_type: FieldType,
    value: &AuraTypedValue,
) -> Result<()> {
    match (declared_type, value) {
        (FieldType::I128, AuraTypedValue::I128(_))
        | (FieldType::Opaque16, AuraTypedValue::Opaque16(_)) => Ok(()),
        (FieldType::I128, AuraTypedValue::I64(_)) => Ok(()),
        (FieldType::TimestampNs | FieldType::I64, AuraTypedValue::I64(_)) => Ok(()),
        (FieldType::I8, AuraTypedValue::I64(value)) => validate_i64_range(
            row_index,
            slot_index,
            declared_type,
            *value,
            i64::from(i8::MIN),
            i64::from(i8::MAX),
        ),
        (FieldType::U8, AuraTypedValue::I64(value)) => validate_i64_range(
            row_index,
            slot_index,
            declared_type,
            *value,
            0,
            i64::from(u8::MAX),
        ),
        (FieldType::I16, AuraTypedValue::I64(value)) => validate_i64_range(
            row_index,
            slot_index,
            declared_type,
            *value,
            i64::from(i16::MIN),
            i64::from(i16::MAX),
        ),
        (FieldType::U16, AuraTypedValue::I64(value)) => validate_i64_range(
            row_index,
            slot_index,
            declared_type,
            *value,
            0,
            i64::from(u16::MAX),
        ),
        (FieldType::I32, AuraTypedValue::I64(value)) => validate_i64_range(
            row_index,
            slot_index,
            declared_type,
            *value,
            i64::from(i32::MIN),
            i64::from(i32::MAX),
        ),
        (FieldType::U32, AuraTypedValue::I64(value)) => validate_i64_range(
            row_index,
            slot_index,
            declared_type,
            *value,
            0,
            i64::from(u32::MAX),
        ),
        (FieldType::U64, AuraTypedValue::I64(value)) => {
            validate_i64_range(row_index, slot_index, declared_type, *value, 0, i64::MAX)
        }
        _ => Err(layout_diagnostic(
            Some(row_index),
            Some(slot_index),
            declared_type.name(),
            value.observed_type(),
            "width mismatch",
            value.observed_value_class(),
            suggested_upgrade(declared_type, value),
        )),
    }
}

fn validate_i64_input(schema: &SchemaDescriptor, rows: &[Vec<i64>]) -> Result<()> {
    for field in &schema.fields {
        if matches!(field.field_type, FieldType::I128 | FieldType::Opaque16) {
            return Err(layout_diagnostic(
                None,
                Some(field.index),
                field.field_type.name(),
                field.field_type.name(),
                "unsupported profile",
                "wide field",
                Some("use typed writer diagnostics until wide-field body support lands"),
            ));
        }
    }
    for (row_index, row) in rows.iter().enumerate() {
        if row.len() != schema.fields.len() {
            return Err(layout_diagnostic(
                Some(row_index),
                None,
                "row",
                "row",
                "field count mismatch",
                "wrong slot count",
                None,
            ));
        }
        for field in &schema.fields {
            validate_typed_value(
                row_index,
                field.index,
                field.field_type,
                &AuraTypedValue::I64(row[usize::from(field.index)]),
            )?;
        }
    }
    Ok(())
}

fn validate_i64_range(
    row_index: usize,
    slot_index: u16,
    declared_type: FieldType,
    value: i64,
    min: i64,
    max: i64,
) -> Result<()> {
    if value < min || value > max {
        return Err(layout_diagnostic(
            Some(row_index),
            Some(slot_index),
            declared_type.name(),
            "i64",
            "overflow",
            "integer",
            Some(suggested_integer_type(value)),
        ));
    }
    Ok(())
}

fn suggested_upgrade(declared_type: FieldType, value: &AuraTypedValue) -> Option<&'static str> {
    match value {
        AuraTypedValue::I64(value) => Some(suggested_integer_type(*value)),
        AuraTypedValue::I128(_) => Some("i128"),
        AuraTypedValue::Opaque16(_) => Some("opaque16"),
    }
    .filter(|suggestion| *suggestion != declared_type.name())
}

fn suggested_integer_type(value: i64) -> &'static str {
    if value >= 0 && value <= i64::from(u8::MAX) {
        "u8"
    } else if value >= i64::from(i8::MIN) && value <= i64::from(i8::MAX) {
        "i8"
    } else if value >= 0 && value <= i64::from(u16::MAX) {
        "u16"
    } else if value >= i64::from(i16::MIN) && value <= i64::from(i16::MAX) {
        "i16"
    } else if value >= 0 && value <= i64::from(u32::MAX) {
        "u32"
    } else if value >= i64::from(i32::MIN) && value <= i64::from(i32::MAX) {
        "i32"
    } else {
        "i64"
    }
}

fn layout_diagnostic(
    row_index: Option<usize>,
    slot_index: Option<u16>,
    declared_type: &'static str,
    observed_type: &'static str,
    reason: &'static str,
    observed_value_class: &'static str,
    suggested_upgrade: Option<&'static str>,
) -> AuraError {
    AuraError::Diagnostic(AuraDiagnostic {
        row_index,
        slot_index,
        declared_type,
        observed_type,
        observed_value_class,
        suggested_upgrade,
        reason,
    })
}
