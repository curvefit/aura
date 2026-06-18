use std::collections::BTreeSet;

use crate::header::{DerivedExpression, DerivedExpressionOp, DerivedExpressionSource};
use crate::records::{self, I64FileInput, TypedFileInput};
use crate::schema::{FieldType, SchemaDescriptor};
use crate::{AuraDiagnostic, AuraError, AuraTypedValue, Profile, Result};

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

/// Declared-layout writer for typed row values.
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
        let internally_derived_slots = schema_internal_derived_slots(&schema);
        Self {
            schema,
            rows: Vec::new(),
            stream_id: 0,
            dictionary_id: 0,
            header_comment: None,
            internally_derived_slots,
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
        let row = if row.len() == self.schema.fields.len() {
            if let Some((slot_index, value)) = row.iter().enumerate().find(|(slot_index, _)| {
                self.internally_derived_slots
                    .contains(&(*slot_index as u16))
            }) {
                let slot_index = u16::try_from(slot_index)
                    .map_err(|_| AuraError::InvalidValue("field index"))?;
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
            row
        } else if !self.internally_derived_slots.is_empty()
            && row.len()
                == self
                    .schema
                    .fields
                    .len()
                    .saturating_sub(self.internally_derived_slots.len())
        {
            self.materialize_internal_row(row_index, row)?
        } else {
            return Err(layout_diagnostic(
                Some(row_index),
                None,
                "row",
                "row",
                "field count mismatch",
                "wrong slot count",
                None,
            ));
        };

        for (slot_index, value) in row.iter().enumerate() {
            let slot_index =
                u16::try_from(slot_index).map_err(|_| AuraError::InvalidValue("field index"))?;
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
        if schema_is_i64_compatible(&self.schema) {
            let rows = typed_rows_to_i64(&self.schema, self.rows)?;
            return records::encode_ingest_i64_file_inner(I64FileInput {
                schema: self.schema,
                rows,
                stream_id: self.stream_id,
                dictionary_id: self.dictionary_id,
                header_comment: self.header_comment,
            });
        }

        records::encode_ingest_typed_file_inner(TypedFileInput {
            schema: self.schema,
            rows: self.rows,
            stream_id: self.stream_id,
            dictionary_id: self.dictionary_id,
            header_comment: self.header_comment,
        })
    }

    fn materialize_internal_row(
        &self,
        row_index: usize,
        row: Vec<AuraTypedValue>,
    ) -> Result<Vec<AuraTypedValue>> {
        let mut values = Vec::new();
        values.resize_with(self.schema.fields.len(), || None);
        let mut supplied = row.into_iter();
        for field in &self.schema.fields {
            if self.internally_derived_slots.contains(&field.index) {
                continue;
            }
            let value = supplied.next().ok_or_else(|| {
                layout_diagnostic(
                    Some(row_index),
                    None,
                    "row",
                    "row",
                    "field count mismatch",
                    "wrong slot count",
                    None,
                )
            })?;
            validate_typed_value(row_index, field.index, field.field_type, &value)?;
            values[usize::from(field.index)] = Some(value);
        }
        if supplied.next().is_some() {
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

        for slot in &self.internally_derived_slots {
            if !self
                .schema
                .derived_expressions
                .iter()
                .any(|expression| expression.output_slot == *slot)
            {
                return Err(layout_diagnostic(
                    Some(row_index),
                    Some(*slot),
                    self.schema.fields[usize::from(*slot)].field_type.name(),
                    "derived",
                    "derived expression missing",
                    "internal slot",
                    Some("declare a derived expression for the internal slot"),
                ));
            }
        }

        for _ in 0..self.internally_derived_slots.len().saturating_add(1) {
            let mut progress = false;
            for expression in &self.schema.derived_expressions {
                if !self
                    .internally_derived_slots
                    .contains(&expression.output_slot)
                    || values[usize::from(expression.output_slot)].is_some()
                {
                    continue;
                }
                if !expression.input_slots.iter().all(|slot| {
                    values
                        .get(usize::from(*slot))
                        .and_then(Option::as_ref)
                        .is_some()
                }) {
                    continue;
                }
                let value = compute_internal_expression(expression, &values)?;
                validate_typed_value(
                    row_index,
                    expression.output_slot,
                    self.schema.fields[usize::from(expression.output_slot)].field_type,
                    &value,
                )?;
                values[usize::from(expression.output_slot)] = Some(value);
                progress = true;
            }
            if values.iter().all(Option::is_some) {
                return values
                    .into_iter()
                    .map(|value| value.ok_or(AuraError::InvalidValue("derived expression")))
                    .collect();
            }
            if !progress {
                break;
            }
        }

        Err(layout_diagnostic(
            Some(row_index),
            None,
            "derived",
            "derived",
            "derived expression dependency",
            "internal slot",
            Some("check internal expression inputs and cycles"),
        ))
    }
}

fn compute_internal_expression(
    expression: &DerivedExpression,
    values: &[Option<AuraTypedValue>],
) -> Result<AuraTypedValue> {
    let mut terms = expression
        .input_slots
        .iter()
        .map(|slot| {
            let value = values
                .get(usize::from(*slot))
                .and_then(Option::as_ref)
                .ok_or(AuraError::InvalidValue("derived expression input"))?;
            typed_i64(value)
        })
        .collect::<Result<Vec<_>>>()?;
    terms.extend_from_slice(&expression.literals);
    let value = match expression.op {
        DerivedExpressionOp::Add => checked_add_terms(&terms)?,
        DerivedExpressionOp::Sub => checked_sub_terms(&terms)?,
        DerivedExpressionOp::Mul => checked_mul_terms(&terms)?,
        DerivedExpressionOp::Div => checked_div_terms(&terms)?,
        DerivedExpressionOp::Min => terms
            .into_iter()
            .min()
            .ok_or(AuraError::InvalidValue("derived expression terms"))?,
        DerivedExpressionOp::Max => terms
            .into_iter()
            .max()
            .ok_or(AuraError::InvalidValue("derived expression terms"))?,
        DerivedExpressionOp::AddResidual
        | DerivedExpressionOp::SubtractResidual
        | DerivedExpressionOp::MaxPlusResidual
        | DerivedExpressionOp::MinMinusResidual
        | DerivedExpressionOp::FirstOffsetThenDelta => {
            return Err(AuraError::InvalidValue("derived expression op"));
        }
    };
    Ok(AuraTypedValue::I64(value))
}

fn typed_i64(value: &AuraTypedValue) -> Result<i64> {
    match value {
        AuraTypedValue::I64(value) => Ok(*value),
        AuraTypedValue::I128(_) => Err(AuraError::InvalidValue("derived expression input")),
        AuraTypedValue::Opaque16(_) => Err(AuraError::InvalidValue("derived expression input")),
    }
}

fn checked_add_terms(terms: &[i64]) -> Result<i64> {
    let value = terms.iter().try_fold(0i128, |value, term| {
        value
            .checked_add(i128::from(*term))
            .ok_or(AuraError::InvalidValue("derived expression value"))
    })?;
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("derived expression value"))
}

fn checked_sub_terms(terms: &[i64]) -> Result<i64> {
    let Some((first, rest)) = terms.split_first() else {
        return Err(AuraError::InvalidValue("derived expression terms"));
    };
    let value = rest.iter().try_fold(i128::from(*first), |value, term| {
        value
            .checked_sub(i128::from(*term))
            .ok_or(AuraError::InvalidValue("derived expression value"))
    })?;
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("derived expression value"))
}

fn checked_mul_terms(terms: &[i64]) -> Result<i64> {
    let value = terms.iter().try_fold(1i128, |value, term| {
        value
            .checked_mul(i128::from(*term))
            .ok_or(AuraError::InvalidValue("derived expression value"))
    })?;
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("derived expression value"))
}

fn checked_div_terms(terms: &[i64]) -> Result<i64> {
    let Some((first, rest)) = terms.split_first() else {
        return Err(AuraError::InvalidValue("derived expression terms"));
    };
    let value = rest.iter().try_fold(i128::from(*first), |value, term| {
        let divisor = i128::from(*term);
        if divisor == 0 {
            return Err(AuraError::InvalidValue("derived expression value"));
        }
        value
            .checked_div(divisor)
            .ok_or(AuraError::InvalidValue("derived expression value"))
    })?;
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("derived expression value"))
}

pub fn encode_i64(input: I64FileInput) -> Result<Vec<u8>> {
    validate_i64_input(&input.schema, &input.rows)?;
    records::encode_ingest_i64_file_inner(input)
}

pub fn encode_typed(input: TypedFileInput) -> Result<Vec<u8>> {
    records::validate_typed_rows(&input.schema, &input.rows)?;
    if schema_is_i64_compatible(&input.schema) {
        let rows = typed_rows_to_i64(&input.schema, input.rows)?;
        return encode_i64(I64FileInput {
            schema: input.schema,
            rows,
            stream_id: input.stream_id,
            dictionary_id: input.dictionary_id,
            header_comment: input.header_comment,
        });
    }
    records::encode_ingest_typed_file_inner(input)
}

fn schema_is_i64_compatible(schema: &SchemaDescriptor) -> bool {
    schema
        .fields
        .iter()
        .all(|field| !matches!(field.field_type, FieldType::I128 | FieldType::Opaque16))
}

fn typed_rows_to_i64(
    schema: &SchemaDescriptor,
    rows: Vec<Vec<AuraTypedValue>>,
) -> Result<Vec<Vec<i64>>> {
    rows.into_iter()
        .enumerate()
        .map(|(row_index, row)| {
            row.into_iter()
                .enumerate()
                .map(|(slot_index, value)| {
                    let slot_index = u16::try_from(slot_index)
                        .map_err(|_| AuraError::InvalidValue("field index"))?;
                    validate_typed_value(
                        row_index,
                        slot_index,
                        schema.fields[usize::from(slot_index)].field_type,
                        &value,
                    )?;
                    match value {
                        AuraTypedValue::I64(value) => Ok(value),
                        AuraTypedValue::I128(value) => {
                            i64::try_from(value).map_err(|_| AuraError::InvalidValue("i128 value"))
                        }
                        AuraTypedValue::Opaque16(_) => Err(AuraError::InvalidValue("opaque value")),
                    }
                })
                .collect()
        })
        .collect()
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
    for expression in &schema.derived_expressions {
        if expression.source() == DerivedExpressionSource::Internal {
            return Err(layout_diagnostic(
                None,
                Some(expression.output_slot),
                "derived",
                "i64",
                "derived slot source conflict",
                "supplied row value",
                Some("use external derived ownership for supplied row slots"),
            ));
        }
    }
    for field in &schema.fields {
        if matches!(field.field_type, FieldType::I128 | FieldType::Opaque16) {
            return Err(layout_diagnostic(
                None,
                Some(field.index),
                field.field_type.name(),
                field.field_type.name(),
                "unsupported profile",
                "wide field",
                Some("use typed ingest for i128 or opaque16 fields"),
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

fn schema_internal_derived_slots(schema: &SchemaDescriptor) -> BTreeSet<u16> {
    schema
        .derived_expressions
        .iter()
        .filter(|expression| expression.source() == DerivedExpressionSource::Internal)
        .map(|expression| expression.output_slot)
        .collect()
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
