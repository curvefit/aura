use std::collections::{BTreeMap, BTreeSet};

use crate::bitpack::{signed_bitpack_width_for_range, unsigned_bitpack_width};
use crate::body::{decode_generic_stream_body, encode_generic_stream_body, GenericStreamBodyValue};
use crate::bytes::{put_u16_le, put_u32_le, put_u64_le, ByteReader};
use crate::instructions::{
    DerivedOp, GenericGroupInstruction, GenericInstructionPlan, GenericStreamInstruction,
    GenericStreamOp,
};
use crate::schema::{FieldRelation, FieldScope, SchemaDescriptor};
use crate::{AuraError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericEncodedI64Rows {
    pub plan: GenericInstructionPlan,
    pub streams: Vec<GenericEncodedStream>,
    pub record_count: usize,
    pub field_count: usize,
}

impl GenericEncodedI64Rows {
    pub fn encoded_body_len(&self) -> usize {
        self.streams.iter().map(|stream| stream.body.len()).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericEncodedStream {
    pub stream_id: u16,
    pub value_count: usize,
    pub body: Vec<u8>,
}

struct PlannedI64Rows {
    plan: GenericInstructionPlan,
}

struct PlannerState {
    streams: Vec<GenericStreamInstruction>,
    groups: Vec<GenericGroupInstruction>,
    planned_slots: BTreeSet<u16>,
    next_stream_id: u16,
    next_group_id: u16,
    repeated_group_id: Option<u16>,
}

impl PlannerState {
    fn new() -> Self {
        Self {
            streams: Vec::new(),
            groups: Vec::new(),
            planned_slots: BTreeSet::new(),
            next_stream_id: 0,
            next_group_id: 0,
            repeated_group_id: None,
        }
    }

    fn add_stream(&mut self, target_slot: Option<u16>, values: Vec<i64>) -> Result<u16> {
        let stream_id = self.next_stream_id;
        self.next_stream_id = self
            .next_stream_id
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("stream id"))?;
        let op = choose_i64_op(&values)?;
        self.streams.push(GenericStreamInstruction {
            stream_id,
            target_slot,
            op,
        });
        Ok(stream_id)
    }

    fn add_derived(
        &mut self,
        output_slot: u16,
        op: DerivedOp,
        input_slots: Vec<u16>,
        values: Vec<i64>,
    ) -> Result<()> {
        let stream_id = self.add_stream(None, values)?;
        let group_id = self.next_group_id;
        self.next_group_id = self
            .next_group_id
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("group id"))?;
        self.groups.push(GenericGroupInstruction::DerivedStream {
            group_id,
            parent_group_id: None,
            output_slot,
            op,
            input_slots,
            stream_id,
        });
        self.planned_slots.insert(output_slot);
        Ok(())
    }

    fn finish(self) -> Result<PlannedI64Rows> {
        let plan = GenericInstructionPlan {
            streams: self.streams,
            groups: self.groups,
        };
        let _encoded = plan.encode()?;
        Ok(PlannedI64Rows { plan })
    }
}

pub fn plan_generic_i64_rows(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
) -> Result<GenericInstructionPlan> {
    Ok(plan_i64_rows(schema, rows)?.plan)
}

pub fn encode_generic_i64_rows(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
) -> Result<GenericEncodedI64Rows> {
    validate_rows(schema, rows)?;
    let planned = plan_i64_rows(schema, rows)?;
    encode_generic_i64_rows_with_plan(schema, rows, planned.plan)
}

pub fn encode_generic_i64_rows_with_plan(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    plan: GenericInstructionPlan,
) -> Result<GenericEncodedI64Rows> {
    validate_rows(schema, rows)?;
    let _encoded_plan = plan.encode()?;
    let streams = plan
        .streams
        .iter()
        .map(|instruction| {
            let values = stream_values_for_instruction(schema, rows, &plan, instruction)?;
            let body = encode_generic_stream_body(
                instruction,
                &GenericStreamBodyValue::I64(values.clone()),
            )?;
            Ok(GenericEncodedStream {
                stream_id: instruction.stream_id,
                value_count: values.len(),
                body,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(GenericEncodedI64Rows {
        plan,
        streams,
        record_count: rows.len(),
        field_count: schema.fields.len(),
    })
}

pub fn encode_generic_i64_rows_body(encoded: &GenericEncodedI64Rows) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    put_u16_len(&mut out, encoded.streams.len(), "generic stream count")?;
    for stream in &encoded.streams {
        put_u16_le(&mut out, stream.stream_id);
        put_u64_le(&mut out, stream.value_count as u64);
        put_u32_len(&mut out, stream.body.len(), "generic stream body length")?;
        out.extend_from_slice(&stream.body);
    }
    Ok(out)
}

pub fn decode_generic_i64_rows_body(
    plan: GenericInstructionPlan,
    bytes: &[u8],
    record_count: usize,
    field_count: usize,
) -> Result<Vec<Vec<i64>>> {
    let mut reader = ByteReader::new(bytes);
    let stream_count = reader.read_u16_le()? as usize;
    let mut streams = Vec::with_capacity(stream_count);
    for _ in 0..stream_count {
        let stream_id = reader.read_u16_le()?;
        let value_count = usize::try_from(reader.read_u64_le()?)
            .map_err(|_| AuraError::InvalidValue("stream value count"))?;
        let body_len = reader.read_u32_le()? as usize;
        let body = reader.read_exact(body_len)?.to_vec();
        streams.push(GenericEncodedStream {
            stream_id,
            value_count,
            body,
        });
    }
    reader.finish()?;
    decode_generic_i64_rows(&GenericEncodedI64Rows {
        plan,
        streams,
        record_count,
        field_count,
    })
}

pub fn decode_generic_i64_rows(encoded: &GenericEncodedI64Rows) -> Result<Vec<Vec<i64>>> {
    let instructions = encoded
        .plan
        .streams
        .iter()
        .map(|instruction| (instruction.stream_id, instruction))
        .collect::<BTreeMap<_, _>>();
    let mut stream_values = BTreeMap::new();
    for stream in &encoded.streams {
        let instruction = instructions
            .get(&stream.stream_id)
            .ok_or(AuraError::InvalidValue("stream id"))?;
        match decode_generic_stream_body(instruction, &stream.body, stream.value_count)? {
            GenericStreamBodyValue::I64(values) => {
                stream_values.insert(stream.stream_id, values);
            }
            GenericStreamBodyValue::U128(_) => return Err(AuraError::InvalidValue("body type")),
        }
    }

    let mut rows = vec![vec![0i64; encoded.field_count]; encoded.record_count];
    let mut filled = vec![vec![false; encoded.field_count]; encoded.record_count];
    for instruction in &encoded.plan.streams {
        let Some(slot) = instruction.target_slot else {
            continue;
        };
        let slot = usize::from(slot);
        if slot >= encoded.field_count {
            return Err(AuraError::InvalidValue("target slot"));
        }
        let values = stream_values
            .get(&instruction.stream_id)
            .ok_or(AuraError::InvalidValue("stream body"))?;
        if values.len() != encoded.record_count {
            return Err(AuraError::InvalidValue("stream value count"));
        }
        for (row_index, value) in values.iter().copied().enumerate() {
            rows[row_index][slot] = value;
            filled[row_index][slot] = true;
        }
    }

    let presence_maps =
        presence_maps_by_group(&encoded.plan, &stream_values, encoded.record_count)?;
    for group in &encoded.plan.groups {
        match group {
            GenericGroupInstruction::SparseStream {
                presence_group_id,
                output_slot,
                presence_index,
                stream_id,
                ..
            } => {
                let output_slot = usize::from(*output_slot);
                if output_slot >= encoded.field_count {
                    return Err(AuraError::InvalidValue("target slot"));
                }
                let masks = presence_maps
                    .get(presence_group_id)
                    .ok_or(AuraError::InvalidValue("presence map reference"))?;
                let values = stream_values
                    .get(stream_id)
                    .ok_or(AuraError::InvalidValue("stream body"))?;
                let mut value_index = 0usize;
                for row_index in 0..encoded.record_count {
                    if presence_bit_set(masks[row_index], *presence_index)? {
                        rows[row_index][output_slot] = *values
                            .get(value_index)
                            .ok_or(AuraError::InvalidValue("sparse stream body"))?;
                        value_index += 1;
                    } else {
                        rows[row_index][output_slot] = 0;
                    }
                    filled[row_index][output_slot] = true;
                }
                if value_index != values.len() {
                    return Err(AuraError::InvalidValue("sparse stream body"));
                }
            }
            GenericGroupInstruction::PresenceValue {
                presence_group_id,
                output_slot,
                presence_index,
                value,
                ..
            } => {
                let output_slot = usize::from(*output_slot);
                if output_slot >= encoded.field_count {
                    return Err(AuraError::InvalidValue("target slot"));
                }
                let masks = presence_maps
                    .get(presence_group_id)
                    .ok_or(AuraError::InvalidValue("presence map reference"))?;
                for row_index in 0..encoded.record_count {
                    rows[row_index][output_slot] =
                        if presence_bit_set(masks[row_index], *presence_index)? {
                            *value
                        } else {
                            0
                        };
                    filled[row_index][output_slot] = true;
                }
            }
            _ => {}
        }
    }

    let derived = encoded
        .plan
        .groups
        .iter()
        .filter_map(|group| match group {
            GenericGroupInstruction::DerivedStream {
                output_slot,
                op,
                input_slots,
                stream_id,
                ..
            } => Some((*output_slot, *op, input_slots.as_slice(), *stream_id)),
            _ => None,
        })
        .collect::<Vec<_>>();

    for _ in 0..encoded.field_count.saturating_mul(2).saturating_add(1) {
        let mut progress = false;
        for row_index in 0..encoded.record_count {
            for (output_slot, op, input_slots, stream_id) in &derived {
                let output_slot = usize::from(*output_slot);
                if output_slot >= encoded.field_count || filled[row_index][output_slot] {
                    continue;
                }
                let values = stream_values
                    .get(stream_id)
                    .ok_or(AuraError::InvalidValue("stream body"))?;
                if values.len() != encoded.record_count {
                    return Err(AuraError::InvalidValue("stream value count"));
                }
                if !derived_inputs_ready(*op, input_slots, row_index, &filled) {
                    continue;
                }
                rows[row_index][output_slot] =
                    derive_value(*op, input_slots, row_index, values[row_index], &rows)?;
                filled[row_index][output_slot] = true;
                progress = true;
            }
        }
        if filled.iter().all(|row| row.iter().all(|slot| *slot)) {
            return Ok(rows);
        }
        if !progress {
            break;
        }
    }

    Err(AuraError::InvalidValue("derived streams"))
}

pub fn plan_uuid_const_mask_stream(
    stream_id: u16,
    target_slot: Option<u16>,
    values: &[u128],
) -> Result<GenericStreamInstruction> {
    let constant_bits = uuid_constant_candidates(values)
        .count_ones()
        .try_into()
        .map_err(|_| AuraError::InvalidValue("uuid bit mask"))?;
    let variable_bits = 128u8
        .checked_sub(constant_bits)
        .ok_or(AuraError::InvalidValue("uuid bit mask"))?;
    let instruction = GenericStreamInstruction {
        stream_id,
        target_slot,
        op: GenericStreamOp::UuidConstMask {
            constant_bits,
            variable_bits,
        },
    };
    let _body =
        encode_generic_stream_body(&instruction, &GenericStreamBodyValue::U128(values.to_vec()))?;
    Ok(instruction)
}

fn stream_values_for_instruction(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    plan: &GenericInstructionPlan,
    instruction: &GenericStreamInstruction,
) -> Result<Vec<i64>> {
    if let Some(slot) = instruction.target_slot {
        return column_values(rows, slot);
    }

    if let Some((output_slot, op, input_slots)) = plan.groups.iter().find_map(|group| match group {
        GenericGroupInstruction::DerivedStream {
            output_slot,
            op,
            input_slots,
            stream_id,
            ..
        } if *stream_id == instruction.stream_id => Some((*output_slot, *op, input_slots)),
        _ => None,
    }) {
        return rows
            .iter()
            .enumerate()
            .map(|(row_index, _)| {
                inverse_derive_value(op, input_slots, output_slot, row_index, rows)
            })
            .collect();
    }

    if let Some(slots) = plan.groups.iter().find_map(|group| match group {
        GenericGroupInstruction::PresenceMap {
            slots, stream_id, ..
        } if *stream_id == instruction.stream_id => Some(slots),
        _ => None,
    }) {
        return rows
            .iter()
            .map(|row| {
                slots
                    .iter()
                    .enumerate()
                    .try_fold(0i64, |mask, (index, slot)| {
                        if row[usize::from(*slot)] == 0 {
                            Ok(mask)
                        } else {
                            let bit = 1i64
                                .checked_shl(
                                    u32::try_from(index)
                                        .map_err(|_| AuraError::InvalidValue("presence bit"))?,
                                )
                                .ok_or(AuraError::InvalidValue("presence bit"))?;
                            Ok(mask | bit)
                        }
                    })
            })
            .collect();
    }

    if let Some(output_slot) = plan.groups.iter().find_map(|group| match group {
        GenericGroupInstruction::SparseStream {
            output_slot,
            stream_id,
            ..
        } if *stream_id == instruction.stream_id => Some(*output_slot),
        _ => None,
    }) {
        let values = rows
            .iter()
            .filter_map(|row| {
                let value = row[usize::from(output_slot)];
                (value != 0).then_some(value)
            })
            .collect::<Vec<_>>();
        return Ok(values);
    }

    if let Some(parent_group_id) = plan.groups.iter().find_map(|group| match group {
        GenericGroupInstruction::PartitionRuns {
            parent_group_id,
            count_stream_id,
            ..
        } if *count_stream_id == instruction.stream_id => Some(*parent_group_id),
        _ => None,
    }) {
        let event_slots = group_event_slots(plan, parent_group_id)?;
        return event_group_lengths(rows, &event_slots);
    }

    let _ = schema;
    Err(AuraError::InvalidValue("generic stream instruction"))
}

fn inverse_derive_value(
    op: DerivedOp,
    input_slots: &[u16],
    output_slot: u16,
    row_index: usize,
    rows: &[Vec<i64>],
) -> Result<i64> {
    let output = rows
        .get(row_index)
        .and_then(|row| row.get(usize::from(output_slot)))
        .copied()
        .ok_or(AuraError::InvalidValue("output slot"))?;
    match op {
        DerivedOp::AddResidual => {
            let base = rows[row_index][usize::from(input_slots[0])];
            checked_delta(output, base)
        }
        DerivedOp::SubtractResidual => {
            let base = rows[row_index][usize::from(input_slots[0])];
            checked_delta(base, output)
        }
        DerivedOp::MaxPlusResidual => {
            let base = input_slots
                .iter()
                .map(|slot| rows[row_index][usize::from(*slot)])
                .max()
                .ok_or(AuraError::InvalidValue("input slots"))?;
            checked_delta(output, base)
        }
        DerivedOp::MinMinusResidual => {
            let base = input_slots
                .iter()
                .map(|slot| rows[row_index][usize::from(*slot)])
                .min()
                .ok_or(AuraError::InvalidValue("input slots"))?;
            checked_delta(base, output)
        }
        DerivedOp::FirstOffsetThenDelta => {
            if row_index == 0 {
                Ok(output)
            } else {
                let base = rows[row_index - 1][usize::from(input_slots[0])];
                checked_delta(output, base)
            }
        }
    }
}

fn group_event_slots(plan: &GenericInstructionPlan, group_id: u16) -> Result<Vec<u16>> {
    plan.groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::Group {
                group_id: candidate,
                event_slots,
                ..
            } if *candidate == group_id => Some(event_slots.clone()),
            _ => None,
        })
        .ok_or(AuraError::InvalidValue("group instruction reference"))
}

fn presence_maps_by_group(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    record_count: usize,
) -> Result<BTreeMap<u16, Vec<i64>>> {
    let mut out = BTreeMap::new();
    for group in &plan.groups {
        let GenericGroupInstruction::PresenceMap {
            group_id,
            slots,
            stream_id,
            ..
        } = group
        else {
            continue;
        };
        if slots.len() > 62 {
            return Err(AuraError::InvalidValue("presence slots"));
        }
        let values = stream_values
            .get(stream_id)
            .ok_or(AuraError::InvalidValue("presence stream body"))?;
        if values.len() != record_count {
            return Err(AuraError::InvalidValue("presence stream body"));
        }
        out.insert(*group_id, values.clone());
    }
    Ok(out)
}

fn presence_bit_set(mask: i64, index: u16) -> Result<bool> {
    if index >= 62 || mask < 0 {
        return Err(AuraError::InvalidValue("presence bit"));
    }
    let bit = 1i64
        .checked_shl(u32::from(index))
        .ok_or(AuraError::InvalidValue("presence bit"))?;
    Ok(mask & bit != 0)
}

fn plan_i64_rows(schema: &SchemaDescriptor, rows: &[Vec<i64>]) -> Result<PlannedI64Rows> {
    validate_rows(schema, rows)?;
    let mut state = PlannerState::new();
    add_group_hints(schema, rows, &mut state)?;
    add_candle_shape_hints(schema, rows, &mut state)?;
    add_sparse_presence_hints(schema, rows, &mut state)?;

    for field in &schema.fields {
        if state.planned_slots.contains(&field.index) {
            continue;
        }
        let values = column_values(rows, field.index)?;
        match field.relation {
            FieldRelation::DeltaFromField(parent_slot) => {
                let parent_values = column_values(rows, parent_slot)?;
                let residuals = values
                    .iter()
                    .zip(parent_values)
                    .map(|(value, parent)| checked_delta(*value, parent))
                    .collect::<Result<Vec<_>>>()?;
                let direct_size = encoded_i64_len(&values)?;
                let residual_size = encoded_i64_len(&residuals)?;
                if residual_size < direct_size {
                    state.add_derived(
                        field.index,
                        DerivedOp::AddResidual,
                        vec![parent_slot],
                        residuals,
                    )?;
                } else {
                    state.add_stream(Some(field.index), values)?;
                    state.planned_slots.insert(field.index);
                }
            }
            FieldRelation::None => {
                state.add_stream(Some(field.index), values)?;
                state.planned_slots.insert(field.index);
            }
        }
    }

    state.finish()
}

fn add_group_hints(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    state: &mut PlannerState,
) -> Result<()> {
    let event_slots = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .map(|field| field.index)
        .collect::<Vec<_>>();
    let repeated_slots = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .map(|field| field.index)
        .collect::<Vec<_>>();
    if repeated_slots.is_empty() {
        return Ok(());
    }

    let group_id = state.next_group_id;
    state.next_group_id = state
        .next_group_id
        .checked_add(1)
        .ok_or(AuraError::InvalidValue("group id"))?;
    state.groups.push(GenericGroupInstruction::Group {
        group_id,
        event_slots: event_slots.clone(),
        repeated_slots: repeated_slots.clone(),
    });
    state.repeated_group_id = Some(group_id);

    if let Some(partition_slot) = fixed_order_partition_slot(rows, &event_slots, &repeated_slots)? {
        let counts = event_group_lengths(rows, &event_slots)?;
        let count_stream_id = state.add_stream(None, counts)?;
        let partition_group_id = state.next_group_id;
        state.next_group_id = state
            .next_group_id
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("group id"))?;
        state.groups.push(GenericGroupInstruction::PartitionRuns {
            group_id: partition_group_id,
            parent_group_id: group_id,
            partition_slot,
            count_stream_id,
            fixed_order: true,
        });
    }

    Ok(())
}

fn add_sparse_presence_hints(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    state: &mut PlannerState,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let Some(parent_group_id) = state.repeated_group_id else {
        return Ok(());
    };
    let mut candidates = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .filter(|field| !state.planned_slots.contains(&field.index))
        .filter_map(|field| sparse_candidate(field.index, rows).transpose())
        .collect::<Result<Vec<_>>>()?;
    if candidates.is_empty() {
        return Ok(());
    }

    candidates.sort_by_key(|candidate| {
        std::cmp::Reverse(candidate.direct_size.saturating_sub(candidate.nonzero_size))
    });
    let mut best: Option<SparseSetCandidate> = None;
    for candidate in &candidates {
        let candidate = sparse_set_candidate(rows, vec![candidate.clone()])?;
        if candidate.sparse_size < candidate.direct_size
            && best
                .as_ref()
                .is_none_or(|current| candidate.sparse_size < current.sparse_size)
        {
            best = Some(candidate);
        }
    }
    let mut selected = Vec::new();
    for candidate in candidates {
        selected.push(candidate);
        let candidate = sparse_set_candidate(rows, selected.clone())?;
        if candidate.sparse_size < candidate.direct_size
            && best
                .as_ref()
                .is_none_or(|current| candidate.sparse_size < current.sparse_size)
        {
            best = Some(candidate);
        }
    }

    let Some(best) = best else {
        return Ok(());
    };
    let presence_values = rows
        .iter()
        .map(|row| {
            best.slots
                .iter()
                .enumerate()
                .try_fold(0i64, |mask, (index, slot)| {
                    if row[usize::from(slot.field_index)] == 0 {
                        Ok(mask)
                    } else {
                        let bit = 1i64
                            .checked_shl(
                                u32::try_from(index)
                                    .map_err(|_| AuraError::InvalidValue("presence bit"))?,
                            )
                            .ok_or(AuraError::InvalidValue("presence bit"))?;
                        Ok(mask | bit)
                    }
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let presence_stream_id = state.add_stream(None, presence_values)?;
    let presence_group_id = state.next_group_id;
    state.next_group_id = state
        .next_group_id
        .checked_add(1)
        .ok_or(AuraError::InvalidValue("group id"))?;
    let presence_slots = best
        .slots
        .iter()
        .map(|slot| slot.field_index)
        .collect::<Vec<_>>();
    state.groups.push(GenericGroupInstruction::PresenceMap {
        group_id: presence_group_id,
        parent_group_id,
        slots: presence_slots,
        stream_id: presence_stream_id,
    });

    for (presence_index, slot) in best.slots.into_iter().enumerate() {
        let group_id = state.next_group_id;
        state.next_group_id = state
            .next_group_id
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("group id"))?;
        if let Some(value) = slot.presence_value {
            state.groups.push(GenericGroupInstruction::PresenceValue {
                group_id,
                parent_group_id,
                presence_group_id,
                output_slot: slot.field_index,
                presence_index: u16::try_from(presence_index)
                    .map_err(|_| AuraError::InvalidValue("presence index"))?,
                value,
            });
        } else {
            let values = rows
                .iter()
                .filter_map(|row| {
                    let value = row[usize::from(slot.field_index)];
                    (value != 0).then_some(value)
                })
                .collect::<Vec<_>>();
            let stream_id = state.add_stream(None, values)?;
            state.groups.push(GenericGroupInstruction::SparseStream {
                group_id,
                parent_group_id,
                presence_group_id,
                output_slot: slot.field_index,
                presence_index: u16::try_from(presence_index)
                    .map_err(|_| AuraError::InvalidValue("presence index"))?,
                stream_id,
            });
        }
        state.planned_slots.insert(slot.field_index);
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct SparseSlotCandidate {
    field_index: u16,
    direct_size: usize,
    nonzero_size: usize,
    presence_value: Option<i64>,
}

#[derive(Debug, Clone)]
struct SparseSetCandidate {
    slots: Vec<SparseSlotCandidate>,
    direct_size: usize,
    sparse_size: usize,
}

fn sparse_candidate(field_index: u16, rows: &[Vec<i64>]) -> Result<Option<SparseSlotCandidate>> {
    let values = column_values(rows, field_index)?;
    let nonzero_values = values
        .iter()
        .copied()
        .filter(|value| *value != 0)
        .collect::<Vec<_>>();
    if nonzero_values.is_empty() || nonzero_values.len() == values.len() {
        return Ok(None);
    }
    let direct_size = encoded_i64_len(&values)?;
    let presence_value = nonzero_values
        .first()
        .copied()
        .filter(|first| nonzero_values.iter().all(|value| value == first));
    let nonzero_size = if presence_value.is_some() {
        0
    } else {
        encoded_i64_len(&nonzero_values)?
    };
    Ok(Some(SparseSlotCandidate {
        field_index,
        direct_size,
        nonzero_size,
        presence_value,
    }))
}

fn sparse_set_candidate(
    rows: &[Vec<i64>],
    slots: Vec<SparseSlotCandidate>,
) -> Result<SparseSetCandidate> {
    if slots.len() > 62 {
        return Err(AuraError::InvalidValue("presence slots"));
    }
    let presence_values = rows
        .iter()
        .map(|row| {
            slots
                .iter()
                .enumerate()
                .try_fold(0i64, |mask, (index, slot)| {
                    if row[usize::from(slot.field_index)] == 0 {
                        Ok(mask)
                    } else {
                        let bit = 1i64
                            .checked_shl(
                                u32::try_from(index)
                                    .map_err(|_| AuraError::InvalidValue("presence bit"))?,
                            )
                            .ok_or(AuraError::InvalidValue("presence bit"))?;
                        Ok(mask | bit)
                    }
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let direct_size = slots.iter().map(|slot| slot.direct_size).sum();
    let sparse_size = encoded_i64_len(&presence_values)?
        + slots.iter().map(|slot| slot.nonzero_size).sum::<usize>();
    Ok(SparseSetCandidate {
        slots,
        direct_size,
        sparse_size,
    })
}

fn add_candle_shape_hints(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    state: &mut PlannerState,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    for parent in &schema.fields {
        if state.planned_slots.contains(&parent.index) || parent.scope != FieldScope::Event {
            continue;
        }
        let children = schema
            .fields
            .iter()
            .filter(|field| field.scope == FieldScope::Event)
            .filter(|field| field.relation.related_field_index() == Some(parent.index))
            .map(|field| field.index)
            .collect::<Vec<_>>();
        if children.len() < 3 {
            continue;
        }
        let Some(candidate) = best_candle_candidate(parent.index, &children, rows)? else {
            continue;
        };
        let candidate_slots = [
            candidate.close_slot,
            candidate.high_slot,
            candidate.low_slot,
        ];
        if candidate.estimated_bytes >= direct_candle_bytes(parent.index, &candidate_slots, rows)? {
            continue;
        }
        state.add_derived(
            parent.index,
            DerivedOp::FirstOffsetThenDelta,
            vec![candidate.close_slot],
            candidate.open_stream,
        )?;
        state.add_derived(
            candidate.close_slot,
            DerivedOp::AddResidual,
            vec![parent.index],
            candidate.close_stream,
        )?;
        state.add_derived(
            candidate.high_slot,
            DerivedOp::MaxPlusResidual,
            vec![parent.index, candidate.close_slot],
            candidate.high_stream,
        )?;
        state.add_derived(
            candidate.low_slot,
            DerivedOp::MinMinusResidual,
            vec![parent.index, candidate.close_slot],
            candidate.low_stream,
        )?;
    }
    Ok(())
}

struct CandleCandidate {
    close_slot: u16,
    high_slot: u16,
    low_slot: u16,
    open_stream: Vec<i64>,
    close_stream: Vec<i64>,
    high_stream: Vec<i64>,
    low_stream: Vec<i64>,
    estimated_bytes: usize,
}

struct CandleColumns<'a> {
    close_slot: u16,
    high_slot: u16,
    low_slot: u16,
    open: &'a [i64],
    close: &'a [i64],
    high: &'a [i64],
    low: &'a [i64],
}

fn best_candle_candidate(
    open_slot: u16,
    children: &[u16],
    rows: &[Vec<i64>],
) -> Result<Option<CandleCandidate>> {
    let open_values = column_values(rows, open_slot)?;
    let mut best = None;
    for close_slot in children {
        let close_values = column_values(rows, *close_slot)?;
        for high_slot in children {
            if high_slot == close_slot {
                continue;
            }
            let high_values = column_values(rows, *high_slot)?;
            for low_slot in children {
                if low_slot == close_slot || low_slot == high_slot {
                    continue;
                }
                let low_values = column_values(rows, *low_slot)?;
                if !is_candle_shape(&open_values, &high_values, &low_values, &close_values) {
                    continue;
                }
                let candidate = candle_candidate(CandleColumns {
                    close_slot: *close_slot,
                    high_slot: *high_slot,
                    low_slot: *low_slot,
                    open: &open_values,
                    close: &close_values,
                    high: &high_values,
                    low: &low_values,
                })?;
                if best.as_ref().is_none_or(|current: &CandleCandidate| {
                    candidate.estimated_bytes < current.estimated_bytes
                }) {
                    best = Some(candidate);
                }
            }
        }
    }
    Ok(best)
}

fn candle_candidate(columns: CandleColumns<'_>) -> Result<CandleCandidate> {
    let mut open_stream = Vec::with_capacity(columns.open.len());
    open_stream.push(columns.open[0]);
    for index in 1..columns.open.len() {
        open_stream.push(checked_delta(
            columns.open[index],
            columns.close[index - 1],
        )?);
    }
    let close_stream = columns
        .close
        .iter()
        .zip(columns.open)
        .map(|(close, open)| checked_delta(*close, *open))
        .collect::<Result<Vec<_>>>()?;
    let high_stream = columns
        .high
        .iter()
        .zip(columns.open)
        .zip(columns.close)
        .map(|((high, open), close)| checked_delta(*high, (*open).max(*close)))
        .collect::<Result<Vec<_>>>()?;
    let low_stream = columns
        .low
        .iter()
        .zip(columns.open)
        .zip(columns.close)
        .map(|((low, open), close)| checked_delta((*open).min(*close), *low))
        .collect::<Result<Vec<_>>>()?;
    let estimated_bytes = encoded_i64_len(&open_stream)?
        + encoded_i64_len(&close_stream)?
        + encoded_i64_len(&high_stream)?
        + encoded_i64_len(&low_stream)?;
    Ok(CandleCandidate {
        close_slot: columns.close_slot,
        high_slot: columns.high_slot,
        low_slot: columns.low_slot,
        open_stream,
        close_stream,
        high_stream,
        low_stream,
        estimated_bytes,
    })
}

fn direct_candle_bytes(open_slot: u16, children: &[u16], rows: &[Vec<i64>]) -> Result<usize> {
    let open_values = column_values(rows, open_slot)?;
    let mut bytes = encoded_i64_len(&open_values)?;
    for child in children {
        let values = column_values(rows, *child)?;
        let residuals = values
            .iter()
            .zip(&open_values)
            .map(|(value, open)| checked_delta(*value, *open))
            .collect::<Result<Vec<_>>>()?;
        bytes += encoded_i64_len(&residuals)?;
    }
    Ok(bytes)
}

fn is_candle_shape(open: &[i64], high: &[i64], low: &[i64], close: &[i64]) -> bool {
    high.iter()
        .zip(low)
        .zip(open)
        .zip(close)
        .all(|(((high, low), open), close)| {
            *high >= (*open).max(*close) && *low <= (*open).min(*close)
        })
}

fn choose_i64_op(values: &[i64]) -> Result<GenericStreamOp> {
    let mut candidates = Vec::new();
    if let Some(op) = derive_fixed_step(values)? {
        candidates.push(op);
    }
    candidates.push(derive_base_bitpack(values)?);
    if let Some(op) = derive_prev_delta(values)? {
        candidates.push(op);
    }
    candidates.push(derive_patched_bitpack(values)?);
    candidates.push(derive_rle(values)?);
    candidates.push(derive_bitplane_rle(values)?);
    if let Some(op) = derive_dictionary(values)? {
        candidates.push(op);
    }
    for block_size in [16usize, 64, 256, 512, 1024, 2048] {
        if values.len() >= block_size {
            let mode_count = values.len().div_ceil(block_size);
            candidates.push(GenericStreamOp::BlockLocal {
                block_size: u16::try_from(block_size)
                    .map_err(|_| AuraError::InvalidValue("block size"))?,
                mode_count: u32::try_from(mode_count)
                    .map_err(|_| AuraError::InvalidValue("block count"))?,
            });
        }
    }

    candidates
        .into_iter()
        .map(|op| {
            let size = encoded_i64_len_with_op(&op, values)?;
            Ok((size, op_preference(&op), op))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .min_by_key(|(size, preference, _)| (*size, *preference))
        .map(|(_, _, op)| op)
        .ok_or(AuraError::InvalidValue("stream op"))
}

fn encoded_i64_len(values: &[i64]) -> Result<usize> {
    let op = choose_i64_op(values)?;
    encoded_i64_len_with_op(&op, values)
}

fn encoded_i64_len_with_op(op: &GenericStreamOp, values: &[i64]) -> Result<usize> {
    let instruction = GenericStreamInstruction {
        stream_id: 0,
        target_slot: Some(0),
        op: op.clone(),
    };
    Ok(
        encode_generic_stream_body(&instruction, &GenericStreamBodyValue::I64(values.to_vec()))?
            .len(),
    )
}

fn op_preference(op: &GenericStreamOp) -> u8 {
    match op {
        GenericStreamOp::FixedStep { .. } => 0,
        GenericStreamOp::BaseBitpack { .. } => 1,
        GenericStreamOp::PrevDelta { .. } => 2,
        GenericStreamOp::PatchedBitpack { .. } => 3,
        GenericStreamOp::Rle { .. } => 4,
        GenericStreamOp::BitplaneRle { .. } => 5,
        GenericStreamOp::Dictionary { .. } => 6,
        GenericStreamOp::BlockLocal { .. } => 7,
        GenericStreamOp::UuidConstMask { .. } => 8,
    }
}

fn derive_fixed_step(values: &[i64]) -> Result<Option<GenericStreamOp>> {
    let Some(base) = values.first().copied() else {
        return Ok(Some(GenericStreamOp::FixedStep { base: 0, step: 0 }));
    };
    let step = if values.len() > 1 {
        match checked_delta(values[1], base) {
            Ok(step) => step,
            Err(_) => return Ok(None),
        }
    } else {
        0
    };
    for (index, value) in values.iter().copied().enumerate() {
        let Ok(expected) = checked_step_value(base, step, index) else {
            return Ok(None);
        };
        if value != expected {
            return Ok(None);
        }
    }
    Ok(Some(GenericStreamOp::FixedStep { base, step }))
}

fn derive_base_bitpack(values: &[i64]) -> Result<GenericStreamOp> {
    let base = values.iter().copied().min().unwrap_or(0);
    let residuals = unsigned_offsets(values, base)?;
    let unit = storage_unit(&residuals);
    let max_scaled = residuals
        .iter()
        .map(|value| value / unit as u64)
        .max()
        .unwrap_or(0);
    Ok(GenericStreamOp::BaseBitpack {
        base,
        unit,
        bit_width: unsigned_bitpack_width(max_scaled),
    })
}

fn derive_prev_delta(values: &[i64]) -> Result<Option<GenericStreamOp>> {
    let Some(base) = values.first().copied() else {
        return Ok(None);
    };
    if values.len() <= 1 {
        return Ok(None);
    }
    let mut deltas = Vec::with_capacity(values.len().saturating_sub(1));
    for pair in values.windows(2) {
        let Ok(delta) = checked_delta(pair[1], pair[0]) else {
            return Ok(None);
        };
        deltas.push(delta);
    }
    let unit = signed_gcd_unit(&deltas);
    let scaled = deltas.iter().map(|delta| *delta / unit).collect::<Vec<_>>();
    let (min, max) = min_max_i64(&scaled).ok_or(AuraError::InvalidValue("previous delta"))?;
    Ok(Some(GenericStreamOp::PrevDelta {
        base,
        unit,
        bit_width: signed_bitpack_width_for_range(min, max),
    }))
}

fn derive_patched_bitpack(values: &[i64]) -> Result<GenericStreamOp> {
    let GenericStreamOp::BaseBitpack {
        base,
        unit,
        bit_width,
    } = derive_base_bitpack(values)?
    else {
        return Err(AuraError::InvalidValue("patched bitpack"));
    };
    let residuals = values
        .iter()
        .map(|value| scaled_unsigned_offset(*value, base, unit))
        .collect::<Result<Vec<_>>>()?;
    let mut best = None;
    for low_width in 0..=bit_width {
        let mut exception_count = 0usize;
        let mut max_high = 0u64;
        for residual in &residuals {
            let high = if low_width == 64 {
                0
            } else {
                *residual >> low_width
            };
            if high != 0 {
                exception_count += 1;
                max_high = max_high.max(high);
            }
        }
        let op = GenericStreamOp::PatchedBitpack {
            base,
            unit,
            low_width,
            high_width: unsigned_bitpack_width(max_high),
            exception_count: u32::try_from(exception_count)
                .map_err(|_| AuraError::InvalidValue("exception count"))?,
        };
        let size = encoded_i64_len_with_op(&op, values)?;
        if best
            .as_ref()
            .is_none_or(|(best_size, _): &(usize, GenericStreamOp)| size < *best_size)
        {
            best = Some((size, op));
        }
    }
    best.map(|(_, op)| op)
        .ok_or(AuraError::InvalidValue("patched bitpack"))
}

fn derive_rle(values: &[i64]) -> Result<GenericStreamOp> {
    let base = values.iter().copied().min().unwrap_or(0);
    let residuals = unsigned_offsets(values, base)?;
    let unit = storage_unit(&residuals);
    let scaled = residuals
        .iter()
        .map(|value| value / unit as u64)
        .collect::<Vec<_>>();
    Ok(GenericStreamOp::Rle {
        base,
        unit,
        bit_width: unsigned_bitpack_width(scaled.iter().copied().max().unwrap_or(0)),
        run_count: u32::try_from(run_count(&scaled))
            .map_err(|_| AuraError::InvalidValue("run count"))?,
    })
}

fn derive_bitplane_rle(values: &[i64]) -> Result<GenericStreamOp> {
    let base = values.iter().copied().min().unwrap_or(0);
    let residuals = unsigned_offsets(values, base)?;
    let unit = storage_unit(&residuals);
    let max_scaled = residuals
        .iter()
        .map(|value| value / unit as u64)
        .max()
        .unwrap_or(0);
    Ok(GenericStreamOp::BitplaneRle {
        base,
        unit,
        bit_width: unsigned_bitpack_width(max_scaled),
    })
}

fn derive_dictionary(values: &[i64]) -> Result<Option<GenericStreamOp>> {
    if values.is_empty() {
        return Ok(None);
    }
    let unit = signed_gcd_unit(values);
    let mut entries = values.iter().map(|value| *value / unit).collect::<Vec<_>>();
    entries.sort_unstable();
    entries.dedup();
    if entries.len() == values.len() {
        return Ok(None);
    }
    let max_code = entries.len().saturating_sub(1) as u64;
    Ok(Some(GenericStreamOp::Dictionary {
        unit,
        entry_count: u32::try_from(entries.len())
            .map_err(|_| AuraError::InvalidValue("dictionary entry count"))?,
        code_width: unsigned_bitpack_width(max_code),
    }))
}

fn fixed_order_partition_slot(
    rows: &[Vec<i64>],
    event_slots: &[u16],
    repeated_slots: &[u16],
) -> Result<Option<u16>> {
    let groups = event_group_ranges(rows, event_slots)?;
    if groups.len() < 2 {
        return Ok(None);
    }
    for slot in repeated_slots {
        let slot_index = usize::from(*slot);
        let first = groups
            .first()
            .map(|(start, end)| {
                rows[*start..*end]
                    .iter()
                    .map(|row| row[slot_index])
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if first.len() < 2 {
            continue;
        }
        let mut unique = first.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() != first.len() || unique.len() > 8 {
            continue;
        }
        let fixed = groups.iter().all(|(start, end)| {
            rows[*start..*end]
                .iter()
                .map(|row| row[slot_index])
                .eq(first.iter().copied())
        });
        if fixed {
            return Ok(Some(*slot));
        }
    }
    Ok(None)
}

fn event_group_lengths(rows: &[Vec<i64>], event_slots: &[u16]) -> Result<Vec<i64>> {
    event_group_ranges(rows, event_slots)?
        .into_iter()
        .map(|(start, end)| {
            i64::try_from(end - start).map_err(|_| AuraError::InvalidValue("group length"))
        })
        .collect()
}

fn event_group_ranges(rows: &[Vec<i64>], event_slots: &[u16]) -> Result<Vec<(usize, usize)>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut groups = Vec::new();
    let mut start = 0usize;
    for index in 1..rows.len() {
        if !same_slots(&rows[start], &rows[index], event_slots)? {
            groups.push((start, index));
            start = index;
        }
    }
    groups.push((start, rows.len()));
    Ok(groups)
}

fn same_slots(left: &[i64], right: &[i64], slots: &[u16]) -> Result<bool> {
    for slot in slots {
        let slot = usize::from(*slot);
        let left = left.get(slot).ok_or(AuraError::InvalidValue("slot"))?;
        let right = right.get(slot).ok_or(AuraError::InvalidValue("slot"))?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn derived_inputs_ready(
    op: DerivedOp,
    input_slots: &[u16],
    row_index: usize,
    filled: &[Vec<bool>],
) -> bool {
    match op {
        DerivedOp::FirstOffsetThenDelta => {
            row_index == 0
                || input_slots
                    .first()
                    .is_some_and(|slot| filled[row_index - 1][usize::from(*slot)])
        }
        _ => input_slots
            .iter()
            .all(|slot| filled[row_index][usize::from(*slot)]),
    }
}

fn derive_value(
    op: DerivedOp,
    input_slots: &[u16],
    row_index: usize,
    residual: i64,
    rows: &[Vec<i64>],
) -> Result<i64> {
    match op {
        DerivedOp::AddResidual => {
            let base = rows[row_index][usize::from(input_slots[0])];
            checked_sum(base, residual)
        }
        DerivedOp::SubtractResidual => {
            let base = rows[row_index][usize::from(input_slots[0])];
            checked_delta(base, residual)
        }
        DerivedOp::MaxPlusResidual => {
            let base = input_slots
                .iter()
                .map(|slot| rows[row_index][usize::from(*slot)])
                .max()
                .ok_or(AuraError::InvalidValue("input slots"))?;
            checked_sum(base, residual)
        }
        DerivedOp::MinMinusResidual => {
            let base = input_slots
                .iter()
                .map(|slot| rows[row_index][usize::from(*slot)])
                .min()
                .ok_or(AuraError::InvalidValue("input slots"))?;
            checked_delta(base, residual)
        }
        DerivedOp::FirstOffsetThenDelta => {
            if row_index == 0 {
                Ok(residual)
            } else {
                let base = rows[row_index - 1][usize::from(input_slots[0])];
                checked_sum(base, residual)
            }
        }
    }
}

fn validate_rows(schema: &SchemaDescriptor, rows: &[Vec<i64>]) -> Result<()> {
    for row in rows {
        if row.len() != schema.fields.len() {
            return Err(AuraError::InvalidValue("row width"));
        }
    }
    Ok(())
}

fn column_values(rows: &[Vec<i64>], field_index: u16) -> Result<Vec<i64>> {
    let field_index = usize::from(field_index);
    rows.iter()
        .map(|row| {
            row.get(field_index)
                .copied()
                .ok_or(AuraError::InvalidValue("field index"))
        })
        .collect()
}

fn unsigned_offsets(values: &[i64], base: i64) -> Result<Vec<u64>> {
    values
        .iter()
        .map(|value| {
            let delta = i128::from(*value) - i128::from(base);
            if delta < 0 {
                return Err(AuraError::InvalidValue("unsigned offset"));
            }
            u64::try_from(delta).map_err(|_| AuraError::InvalidValue("unsigned offset"))
        })
        .collect()
}

fn scaled_unsigned_offset(value: i64, base: i64, unit: i64) -> Result<u64> {
    if unit <= 0 {
        return Err(AuraError::InvalidValue("storage unit"));
    }
    let delta = i128::from(value) - i128::from(base);
    if delta < 0 || delta % i128::from(unit) != 0 {
        return Err(AuraError::InvalidValue("scaled value"));
    }
    u64::try_from(delta / i128::from(unit)).map_err(|_| AuraError::InvalidValue("scaled value"))
}

fn checked_delta(value: i64, base: i64) -> Result<i64> {
    let delta = i128::from(value) - i128::from(base);
    i64::try_from(delta).map_err(|_| AuraError::InvalidValue("delta"))
}

fn checked_sum(base: i64, delta: i64) -> Result<i64> {
    let sum = i128::from(base) + i128::from(delta);
    i64::try_from(sum).map_err(|_| AuraError::InvalidValue("sum"))
}

fn checked_step_value(base: i64, step: i64, index: usize) -> Result<i64> {
    let value = i128::from(base)
        + i128::from(step)
            .checked_mul(i128::try_from(index).map_err(|_| AuraError::InvalidValue("index"))?)
            .ok_or(AuraError::InvalidValue("fixed step"))?;
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("fixed step"))
}

fn min_max_i64(values: &[i64]) -> Option<(i64, i64)> {
    let mut iter = values.iter().copied();
    let first = iter.next()?;
    Some(iter.fold((first, first), |(min, max), value| {
        (min.min(value), max.max(value))
    }))
}

fn signed_gcd_unit(values: &[i64]) -> i64 {
    let mut out = 0u64;
    for value in values.iter().copied().filter(|value| *value != 0) {
        let abs = if value < 0 {
            u64::try_from(-i128::from(value)).unwrap_or(u64::MAX)
        } else {
            value as u64
        };
        out = if out == 0 { abs } else { gcd(out, abs) };
    }
    i64::try_from(out.max(1)).unwrap_or(i64::MAX)
}

fn gcd_unit(values: &[u64]) -> u64 {
    let mut out = 0u64;
    for value in values.iter().copied().filter(|value| *value != 0) {
        out = if out == 0 { value } else { gcd(out, value) };
    }
    out.max(1)
}

fn storage_unit(values: &[u64]) -> i64 {
    let unit = gcd_unit(values);
    i64::try_from(unit).unwrap_or(1)
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let next = left % right;
        left = right;
        right = next;
    }
    left
}

fn run_count<T: Eq>(values: &[T]) -> usize {
    let Some(first) = values.first() else {
        return 0;
    };
    let mut count = 1usize;
    let mut previous = first;
    for value in &values[1..] {
        if value != previous {
            count += 1;
            previous = value;
        }
    }
    count
}

fn uuid_constant_candidates(values: &[u128]) -> u128 {
    if values.is_empty() {
        return u128::MAX;
    }
    let mut all_ones = u128::MAX;
    let mut all_zeroes = u128::MAX;
    for value in values {
        all_ones &= *value;
        all_zeroes &= !*value;
    }
    all_ones | all_zeroes
}

fn put_u16_len(out: &mut Vec<u8>, len: usize, name: &'static str) -> Result<()> {
    let len = u16::try_from(len).map_err(|_| AuraError::InvalidValue(name))?;
    put_u16_le(out, len);
    Ok(())
}

fn put_u32_len(out: &mut Vec<u8>, len: usize, name: &'static str) -> Result<()> {
    let len = u32::try_from(len).map_err(|_| AuraError::InvalidValue(name))?;
    put_u32_le(out, len);
    Ok(())
}
