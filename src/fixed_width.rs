use crate::program::CompiledAuraField;
use crate::{AuraError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixedLoadKind {
    I8,
    I16,
    I32,
    I64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixedFieldLoad {
    pub field_index: u16,
    pub offset: usize,
    pub kind: FixedLoadKind,
}

impl FixedFieldLoad {
    pub(crate) fn from_field(field: CompiledAuraField) -> Result<Self> {
        let kind = match field.width {
            1 => FixedLoadKind::I8,
            2 => FixedLoadKind::I16,
            4 => FixedLoadKind::I32,
            8 => FixedLoadKind::I64,
            _ => return Err(AuraError::InvalidValue("fixed-width load width")),
        };
        Ok(Self {
            field_index: field.field_index,
            offset: field.offset,
            kind,
        })
    }
}

pub(crate) fn fixed_field_loads(fields: &[CompiledAuraField]) -> Result<Vec<FixedFieldLoad>> {
    fields
        .iter()
        .copied()
        .map(FixedFieldLoad::from_field)
        .collect()
}

pub(crate) fn validate_fixed_body(
    body: &[u8],
    record_width: usize,
    row_count: usize,
    fields: &[CompiledAuraField],
) -> Result<()> {
    let expected_len = record_width
        .checked_mul(row_count)
        .ok_or(AuraError::InvalidValue("body length"))?;
    if body.len() != expected_len {
        return Err(AuraError::UnexpectedEof);
    }
    for field in fields {
        let end = field
            .offset
            .checked_add(field.width)
            .ok_or(AuraError::InvalidValue("field offset"))?;
        if end > record_width {
            return Err(AuraError::UnexpectedEof);
        }
        FixedFieldLoad::from_field(*field)?;
    }
    Ok(())
}

#[inline(always)]
pub(crate) fn parse_checksum_value(
    checksum: u64,
    row_index: usize,
    field_index: u16,
    value: i64,
) -> u64 {
    let row_tag = (row_index as u64).rotate_left(17);
    let field_tag = (u64::from(field_index)).rotate_left(41);
    checksum.wrapping_add((value as u64) ^ row_tag ^ field_tag)
}

#[inline(always)]
pub(crate) fn read_i64_checked(row: &[u8], field: CompiledAuraField) -> Result<i64> {
    let end = field
        .offset
        .checked_add(field.width)
        .ok_or(AuraError::InvalidValue("field offset"))?;
    let bytes = row.get(field.offset..end).ok_or(AuraError::UnexpectedEof)?;
    match bytes.len() {
        1 => Ok(bytes[0] as i8 as i64),
        2 => Ok(i16::from_le_bytes([bytes[0], bytes[1]]) as i64),
        4 => Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i64),
        8 => Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        _ => Err(AuraError::InvalidValue("fixed-width load width")),
    }
}

/// Read a prevalidated fixed-width Aura1 field from `row_ptr`.
///
/// # Safety
///
/// Callers must validate that every row has at least `record_width` bytes, that
/// `load.offset + load.width <= record_width`, and that `row_ptr` points at the
/// beginning of one valid row. `validate_fixed_body` plus loads created by
/// `fixed_field_loads` establish those invariants for current Aura1 batch views.
#[inline(always)]
pub(crate) unsafe fn read_i64_unchecked(row_ptr: *const u8, load: FixedFieldLoad) -> i64 {
    match load.kind {
        FixedLoadKind::I8 => unsafe {
            std::ptr::read_unaligned(row_ptr.add(load.offset) as *const i8) as i64
        },
        FixedLoadKind::I16 => {
            let raw = unsafe { std::ptr::read_unaligned(row_ptr.add(load.offset) as *const u16) };
            u16::from_le(raw) as i16 as i64
        }
        FixedLoadKind::I32 => {
            let raw = unsafe { std::ptr::read_unaligned(row_ptr.add(load.offset) as *const u32) };
            u32::from_le(raw) as i32 as i64
        }
        FixedLoadKind::I64 => {
            let raw = unsafe { std::ptr::read_unaligned(row_ptr.add(load.offset) as *const u64) };
            u64::from_le(raw) as i64
        }
    }
}
