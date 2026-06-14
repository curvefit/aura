use crate::bytes::ByteReader;
use crate::schema::{FieldScope, SchemaDescriptor};
use crate::varint;
use crate::{AuraError, Result};

const MAGIC: &[u8; 4] = b"AURG";
const VERSION: u8 = 1;

/// Row grouping derived from the compact schema header map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedI64Plan {
    pub event_slots: Vec<usize>,
    pub repeated_slots: Vec<usize>,
}

pub fn plan_from_schema(schema: &SchemaDescriptor) -> Result<ScopedI64Plan> {
    if schema.fields.is_empty() {
        return Err(AuraError::InvalidValue("schema fields"));
    }
    let mut event_slots = Vec::new();
    let mut repeated_slots = Vec::new();
    for (position, field) in schema.fields.iter().enumerate() {
        if usize::from(field.index) != position {
            return Err(AuraError::InvalidValue("schema field index"));
        }
        match field.scope {
            FieldScope::Event => event_slots.push(position),
            FieldScope::Repeated => repeated_slots.push(position),
        }
    }
    if event_slots.is_empty() {
        return Err(AuraError::InvalidValue("event slots"));
    }
    Ok(ScopedI64Plan {
        event_slots,
        repeated_slots,
    })
}

/// Encode flat rows as event groups using only schema scope metadata.
///
/// Event slots are written once per contiguous event group. Repeated slots are
/// written once per child row and delta-varinted inside the group. Per-slot
/// storage units are derived from the rows and stored in the stream header so
/// decode is exact without source-specific scaling knowledge.
pub fn encode_grouped_i64_rows(schema: &SchemaDescriptor, rows: &[Vec<i64>]) -> Result<Vec<u8>> {
    validate_rows(schema, rows)?;
    let plan = plan_from_schema(schema)?;
    let units = slot_units(schema.fields.len(), rows)?;
    let groups = row_groups(rows, &plan);

    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    varint::encode_u64(schema.fields.len() as u64, &mut out);
    varint::encode_u64(rows.len() as u64, &mut out);
    for unit in &units {
        varint::encode_i64(*unit, &mut out);
    }
    varint::encode_u64(groups.len() as u64, &mut out);

    let mut previous_event = vec![0i64; schema.fields.len()];
    for group in groups {
        let child_count = group.end - group.start;
        varint::encode_u64(child_count as u64, &mut out);
        let first_row = &rows[group.start];
        for slot in &plan.event_slots {
            let scaled = scaled_value(first_row[*slot], units[*slot])?;
            let delta = checked_delta(scaled, previous_event[*slot])?;
            varint::encode_i64(delta, &mut out);
            previous_event[*slot] = scaled;
        }

        let mut previous_repeated = vec![0i64; plan.repeated_slots.len()];
        for row in &rows[group.start..group.end] {
            for (position, slot) in plan.repeated_slots.iter().enumerate() {
                let scaled = scaled_value(row[*slot], units[*slot])?;
                let delta = checked_delta(scaled, previous_repeated[position])?;
                varint::encode_i64(delta, &mut out);
                previous_repeated[position] = scaled;
            }
        }
    }

    Ok(out)
}

pub fn decode_grouped_i64_rows(schema: &SchemaDescriptor, bytes: &[u8]) -> Result<Vec<Vec<i64>>> {
    let plan = plan_from_schema(schema)?;
    let mut reader = ByteReader::new(bytes);
    if reader.read_exact(4)? != MAGIC {
        return Err(AuraError::InvalidMagic { expected: "AURG" });
    }
    let version = reader.read_u8()?;
    if version != VERSION {
        return Err(AuraError::UnsupportedVersion(u16::from(version)));
    }
    let field_count = usize::try_from(varint::decode_u64(&mut reader)?)
        .map_err(|_| AuraError::InvalidValue("field count"))?;
    if field_count != schema.fields.len() {
        return Err(AuraError::InvalidValue("field count"));
    }
    let row_count = usize::try_from(varint::decode_u64(&mut reader)?)
        .map_err(|_| AuraError::InvalidValue("record count"))?;
    let mut units = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let unit = varint::decode_i64(&mut reader)?;
        if unit <= 0 {
            return Err(AuraError::InvalidValue("storage unit"));
        }
        units.push(unit);
    }

    let group_count = usize::try_from(varint::decode_u64(&mut reader)?)
        .map_err(|_| AuraError::InvalidValue("group count"))?;
    let mut rows = Vec::with_capacity(row_count);
    let mut previous_event = vec![0i64; field_count];
    for _ in 0..group_count {
        let child_count = usize::try_from(varint::decode_u64(&mut reader)?)
            .map_err(|_| AuraError::InvalidValue("child count"))?;
        let mut event_values = vec![0i64; field_count];
        for slot in &plan.event_slots {
            let scaled = checked_sum(previous_event[*slot], varint::decode_i64(&mut reader)?)?;
            previous_event[*slot] = scaled;
            event_values[*slot] = unscaled_value(scaled, units[*slot])?;
        }

        let mut previous_repeated = vec![0i64; plan.repeated_slots.len()];
        for _ in 0..child_count {
            let mut row = vec![0i64; field_count];
            for slot in &plan.event_slots {
                row[*slot] = event_values[*slot];
            }
            for (position, slot) in plan.repeated_slots.iter().enumerate() {
                let scaled = checked_sum(
                    previous_repeated[position],
                    varint::decode_i64(&mut reader)?,
                )?;
                previous_repeated[position] = scaled;
                row[*slot] = unscaled_value(scaled, units[*slot])?;
            }
            rows.push(row);
        }
    }
    if rows.len() != row_count {
        return Err(AuraError::InvalidValue("record count"));
    }
    reader.finish()?;
    Ok(rows)
}

#[derive(Debug, Clone, Copy)]
struct RowGroup {
    start: usize,
    end: usize,
}

fn row_groups(rows: &[Vec<i64>], plan: &ScopedI64Plan) -> Vec<RowGroup> {
    if plan.repeated_slots.is_empty() {
        return (0..rows.len())
            .map(|idx| RowGroup {
                start: idx,
                end: idx + 1,
            })
            .collect();
    }

    let mut groups = Vec::new();
    let mut start = 0usize;
    while start < rows.len() {
        let mut end = start + 1;
        while end < rows.len() && same_event(&rows[start], &rows[end], &plan.event_slots) {
            end += 1;
        }
        groups.push(RowGroup { start, end });
        start = end;
    }
    groups
}

fn same_event(left: &[i64], right: &[i64], event_slots: &[usize]) -> bool {
    event_slots.iter().all(|slot| left[*slot] == right[*slot])
}

fn slot_units(field_count: usize, rows: &[Vec<i64>]) -> Result<Vec<i64>> {
    let mut units = vec![0u64; field_count];
    for row in rows {
        for (slot, value) in row.iter().copied().enumerate() {
            let value = value.unsigned_abs();
            if value == 0 {
                continue;
            }
            units[slot] = if units[slot] == 0 {
                value
            } else {
                gcd(units[slot], value)
            };
        }
    }
    units
        .into_iter()
        .map(|unit| {
            if unit <= 1 || unit > i64::MAX as u64 {
                Ok(1)
            } else {
                Ok(unit as i64)
            }
        })
        .collect()
}

fn scaled_value(value: i64, unit: i64) -> Result<i64> {
    if unit <= 0 || value % unit != 0 {
        return Err(AuraError::InvalidValue("storage unit"));
    }
    Ok(value / unit)
}

fn unscaled_value(value: i64, unit: i64) -> Result<i64> {
    value
        .checked_mul(unit)
        .ok_or(AuraError::InvalidValue("storage unit"))
}

fn checked_delta(value: i64, previous: i64) -> Result<i64> {
    value
        .checked_sub(previous)
        .ok_or(AuraError::InvalidValue("delta value"))
}

fn checked_sum(value: i64, delta: i64) -> Result<i64> {
    value
        .checked_add(delta)
        .ok_or(AuraError::InvalidValue("delta value"))
}

fn validate_rows(schema: &SchemaDescriptor, rows: &[Vec<i64>]) -> Result<()> {
    for row in rows {
        if row.len() != schema.fields.len() {
            return Err(AuraError::InvalidValue("record field count"));
        }
    }
    Ok(())
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let next = left % right;
        left = right;
        right = next;
    }
    left
}
