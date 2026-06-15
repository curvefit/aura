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

#[derive(Debug, Clone)]
struct PartitionRunPlan {
    group_id: u16,
    partition_slot: u16,
    fixed_order_len: Option<usize>,
    has_event_run_counts: bool,
    runs: Vec<PartitionRun>,
}

#[derive(Debug, Clone, Copy)]
struct PartitionRun {
    start: usize,
    end: usize,
    value: i64,
}

#[derive(Debug)]
struct PartitionRunCandidate {
    partition_slot: u16,
    fixed_order_values: Option<Vec<i64>>,
    event_run_counts: Vec<i64>,
    runs: Vec<PartitionRun>,
}

struct SegmentedDeltaCandidate {
    output_slot: u16,
    base_values: Option<Vec<i64>>,
    first_values: Vec<i64>,
    delta_values: Vec<i64>,
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

    let partition_runs = materialize_partition_run_lengths(
        &encoded.plan,
        &stream_values,
        encoded.record_count,
        encoded.field_count,
        &mut rows,
        &mut filled,
    )?;
    materialize_group_value_streams(
        &encoded.plan,
        &stream_values,
        &partition_runs,
        encoded.field_count,
        &mut rows,
        &mut filled,
    )?;
    materialize_segmented_delta_streams(
        &encoded.plan,
        &stream_values,
        &partition_runs,
        encoded.field_count,
        &mut rows,
        &mut filled,
    )?;

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

    if let Some((parent_group_id, partition_slot, fixed_order, stream_kind)) =
        plan.groups.iter().find_map(|group| match group {
            GenericGroupInstruction::PartitionRunLengths {
                parent_group_id,
                partition_slot,
                fixed_order,
                value_stream_id,
                count_stream_id,
                event_count_stream_id: _,
                ..
            } if *value_stream_id == instruction.stream_id => {
                Some((*parent_group_id, *partition_slot, *fixed_order, 0u8))
            }
            GenericGroupInstruction::PartitionRunLengths {
                parent_group_id,
                partition_slot,
                fixed_order,
                value_stream_id: _,
                count_stream_id,
                event_count_stream_id: _,
                ..
            } if *count_stream_id == instruction.stream_id => {
                Some((*parent_group_id, *partition_slot, *fixed_order, 1u8))
            }
            GenericGroupInstruction::PartitionRunLengths {
                parent_group_id,
                partition_slot,
                fixed_order,
                value_stream_id: _,
                count_stream_id: _,
                event_count_stream_id: Some(event_count_stream_id),
                ..
            } if *event_count_stream_id == instruction.stream_id => {
                Some((*parent_group_id, *partition_slot, *fixed_order, 2u8))
            }
            _ => None,
        })
    {
        let event_slots = group_event_slots(plan, parent_group_id)?;
        return match stream_kind {
            0 if fixed_order => fixed_partition_run_order(rows, &event_slots, partition_slot)?
                .ok_or(AuraError::InvalidValue("fixed partition order")),
            0 => Ok(partition_run_ranges(rows, &event_slots, partition_slot)?
                .into_iter()
                .map(|run| run.value)
                .collect()),
            1 => partition_run_ranges(rows, &event_slots, partition_slot)?
                .into_iter()
                .map(|run| {
                    i64::try_from(run.end - run.start)
                        .map_err(|_| AuraError::InvalidValue("run length"))
                })
                .collect(),
            2 => partition_run_event_counts(rows, &event_slots, partition_slot),
            _ => Err(AuraError::InvalidValue("partition stream")),
        };
    }

    if let Some((parent_group_id, output_slot, stream_kind, has_base_stream)) =
        plan.groups.iter().find_map(|group| match group {
            GenericGroupInstruction::SegmentedDeltaStream {
                parent_group_id,
                output_slot,
                base_stream_id,
                first_stream_id,
                delta_stream_id,
                ..
            } if base_stream_id == &Some(instruction.stream_id) => {
                Some((*parent_group_id, *output_slot, 0u8, true))
            }
            GenericGroupInstruction::SegmentedDeltaStream {
                parent_group_id,
                output_slot,
                base_stream_id,
                first_stream_id,
                delta_stream_id: _,
                ..
            } if *first_stream_id == instruction.stream_id => Some((
                *parent_group_id,
                *output_slot,
                1u8,
                base_stream_id.is_some(),
            )),
            GenericGroupInstruction::SegmentedDeltaStream {
                parent_group_id,
                output_slot,
                base_stream_id: _,
                first_stream_id: _,
                delta_stream_id,
                ..
            } if *delta_stream_id == instruction.stream_id => {
                Some((*parent_group_id, *output_slot, 2u8, false))
            }
            _ => None,
        })
    {
        let runs = partition_runs_for_group(plan, rows, parent_group_id)?;
        return match stream_kind {
            0 => Ok(partition_value_bases(rows, &runs, output_slot)?
                .into_values()
                .collect()),
            1 => {
                let output_slot = usize::from(output_slot);
                if has_base_stream {
                    let bases = partition_value_bases(
                        rows,
                        &runs,
                        u16::try_from(output_slot)
                            .map_err(|_| AuraError::InvalidValue("target slot"))?,
                    )?;
                    runs.iter()
                        .map(|run| {
                            let base = bases
                                .get(&run.value)
                                .copied()
                                .ok_or(AuraError::InvalidValue("partition base"))?;
                            checked_delta(rows[run.start][output_slot], base)
                        })
                        .collect()
                } else {
                    Ok(runs
                        .iter()
                        .map(|run| rows[run.start][output_slot])
                        .collect())
                }
            }
            2 => {
                let mut deltas = Vec::with_capacity(rows.len().saturating_sub(runs.len()));
                let output_slot = usize::from(output_slot);
                for run in runs {
                    for row_index in run.start + 1..run.end {
                        deltas.push(checked_delta(
                            rows[row_index][output_slot],
                            rows[row_index - 1][output_slot],
                        )?);
                    }
                }
                Ok(deltas)
            }
            _ => Err(AuraError::InvalidValue("segmented stream")),
        };
    }

    if let Some((parent_group_id, output_slot)) = plan.groups.iter().find_map(|group| match group {
        GenericGroupInstruction::GroupValueStream {
            parent_group_id,
            output_slot,
            stream_id,
            ..
        } if *stream_id == instruction.stream_id => Some((*parent_group_id, *output_slot)),
        _ => None,
    }) {
        let parent_group_id = partition_parent_group_id(plan, parent_group_id)?;
        let event_slots = group_event_slots(plan, parent_group_id)?;
        return event_group_ranges(rows, &event_slots)?
            .into_iter()
            .map(|(start, _)| {
                rows.get(start)
                    .and_then(|row| row.get(usize::from(output_slot)))
                    .copied()
                    .ok_or(AuraError::InvalidValue("group value stream"))
            })
            .collect();
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

fn partition_runs_for_group(
    plan: &GenericInstructionPlan,
    rows: &[Vec<i64>],
    group_id: u16,
) -> Result<Vec<PartitionRun>> {
    let (parent_group_id, partition_slot) = plan
        .groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::PartitionRunLengths {
                group_id: candidate,
                parent_group_id,
                partition_slot,
                ..
            } if *candidate == group_id => Some((*parent_group_id, *partition_slot)),
            _ => None,
        })
        .ok_or(AuraError::InvalidValue("partition run reference"))?;
    let event_slots = group_event_slots(plan, parent_group_id)?;
    partition_run_ranges(rows, &event_slots, partition_slot)
}

fn partition_parent_group_id(plan: &GenericInstructionPlan, group_id: u16) -> Result<u16> {
    plan.groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::PartitionRunLengths {
                group_id: candidate,
                parent_group_id,
                ..
            } if *candidate == group_id => Some(*parent_group_id),
            _ => None,
        })
        .ok_or(AuraError::InvalidValue("partition run reference"))
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

fn materialize_partition_run_lengths(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    record_count: usize,
    field_count: usize,
    rows: &mut [Vec<i64>],
    filled: &mut [Vec<bool>],
) -> Result<BTreeMap<u16, Vec<PartitionRun>>> {
    let mut out = BTreeMap::new();
    for group in &plan.groups {
        let GenericGroupInstruction::PartitionRunLengths {
            group_id,
            partition_slot,
            fixed_order,
            value_stream_id,
            count_stream_id,
            ..
        } = group
        else {
            continue;
        };
        let partition_slot = usize::from(*partition_slot);
        if partition_slot >= field_count {
            return Err(AuraError::InvalidValue("partition slot"));
        }
        let values = stream_values
            .get(value_stream_id)
            .ok_or(AuraError::InvalidValue("partition value stream"))?;
        let counts = stream_values
            .get(count_stream_id)
            .ok_or(AuraError::InvalidValue("partition count stream"))?;
        if *fixed_order {
            if !counts.is_empty() && values.is_empty() {
                return Err(AuraError::InvalidValue("fixed partition order"));
            }
        } else if values.len() != counts.len() {
            return Err(AuraError::InvalidValue("partition stream length"));
        }

        let mut row_index = 0usize;
        let mut runs = Vec::with_capacity(counts.len());
        for (run_index, count) in counts.iter().copied().enumerate() {
            if count <= 0 {
                return Err(AuraError::InvalidValue("partition run length"));
            }
            let count = usize::try_from(count)
                .map_err(|_| AuraError::InvalidValue("partition run length"))?;
            let end = row_index
                .checked_add(count)
                .ok_or(AuraError::InvalidValue("partition run length"))?;
            if end > record_count {
                return Err(AuraError::InvalidValue("partition run length"));
            }
            let value = if *fixed_order {
                values[run_index % values.len()]
            } else {
                values[run_index]
            };
            for row in row_index..end {
                rows[row][partition_slot] = value;
                filled[row][partition_slot] = true;
            }
            runs.push(PartitionRun {
                start: row_index,
                end,
                value,
            });
            row_index = end;
        }
        if row_index != record_count {
            return Err(AuraError::InvalidValue("partition run length"));
        }
        out.insert(*group_id, runs);
    }
    Ok(out)
}

fn materialize_group_value_streams(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    partition_runs: &BTreeMap<u16, Vec<PartitionRun>>,
    field_count: usize,
    rows: &mut [Vec<i64>],
    filled: &mut [Vec<bool>],
) -> Result<()> {
    for group in &plan.groups {
        let GenericGroupInstruction::GroupValueStream {
            parent_group_id,
            output_slot,
            stream_id,
            ..
        } = group
        else {
            continue;
        };
        let output_slot = usize::from(*output_slot);
        if output_slot >= field_count {
            return Err(AuraError::InvalidValue("target slot"));
        }
        let event_ranges = event_ranges_from_partition_runs(
            plan,
            stream_values,
            partition_runs,
            *parent_group_id,
        )?;
        let values = stream_values
            .get(stream_id)
            .ok_or(AuraError::InvalidValue("group value stream"))?;
        if values.len() != event_ranges.len() {
            return Err(AuraError::InvalidValue("group value stream"));
        }
        for ((start, end), value) in event_ranges.into_iter().zip(values.iter().copied()) {
            for row_index in start..end {
                rows[row_index][output_slot] = value;
                filled[row_index][output_slot] = true;
            }
        }
    }
    Ok(())
}

fn materialize_segmented_delta_streams(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    partition_runs: &BTreeMap<u16, Vec<PartitionRun>>,
    field_count: usize,
    rows: &mut [Vec<i64>],
    filled: &mut [Vec<bool>],
) -> Result<()> {
    for group in &plan.groups {
        let GenericGroupInstruction::SegmentedDeltaStream {
            parent_group_id,
            output_slot,
            base_stream_id,
            first_stream_id,
            delta_stream_id,
            ..
        } = group
        else {
            continue;
        };
        let output_slot = usize::from(*output_slot);
        if output_slot >= field_count {
            return Err(AuraError::InvalidValue("target slot"));
        }
        let runs = partition_runs
            .get(parent_group_id)
            .ok_or(AuraError::InvalidValue("partition run reference"))?;
        let first_values = stream_values
            .get(first_stream_id)
            .ok_or(AuraError::InvalidValue("segmented first stream"))?;
        let delta_values = stream_values
            .get(delta_stream_id)
            .ok_or(AuraError::InvalidValue("segmented delta stream"))?;
        if first_values.len() != runs.len() {
            return Err(AuraError::InvalidValue("segmented first stream"));
        }
        let base_by_partition = if let Some(base_stream_id) = base_stream_id {
            let base_values = stream_values
                .get(base_stream_id)
                .ok_or(AuraError::InvalidValue("segmented base stream"))?;
            let partition_values = partition_values_from_runs(runs);
            if base_values.len() != partition_values.len() {
                return Err(AuraError::InvalidValue("segmented base stream"));
            }
            Some(
                partition_values
                    .into_iter()
                    .zip(base_values.iter().copied())
                    .collect::<BTreeMap<_, _>>(),
            )
        } else {
            None
        };
        let mut delta_index = 0usize;
        for (run, first_value) in runs.iter().zip(first_values.iter().copied()) {
            if run.start >= run.end || run.end > rows.len() {
                return Err(AuraError::InvalidValue("partition run"));
            }
            let mut value = if let Some(base_by_partition) = &base_by_partition {
                let base = base_by_partition
                    .get(&run.value)
                    .copied()
                    .ok_or(AuraError::InvalidValue("segmented base stream"))?;
                checked_sum(base, first_value)?
            } else {
                first_value
            };
            rows[run.start][output_slot] = value;
            filled[run.start][output_slot] = true;
            for row_index in run.start + 1..run.end {
                let delta = *delta_values
                    .get(delta_index)
                    .ok_or(AuraError::InvalidValue("segmented delta stream"))?;
                value = checked_sum(value, delta)?;
                rows[row_index][output_slot] = value;
                filled[row_index][output_slot] = true;
                delta_index += 1;
            }
        }
        if delta_index != delta_values.len() {
            return Err(AuraError::InvalidValue("segmented delta stream"));
        }
    }
    Ok(())
}

fn partition_values_from_runs(runs: &[PartitionRun]) -> Vec<i64> {
    let mut values = runs.iter().map(|run| run.value).collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    values
}

fn event_ranges_from_partition_runs(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    partition_runs: &BTreeMap<u16, Vec<PartitionRun>>,
    group_id: u16,
) -> Result<Vec<(usize, usize)>> {
    let (fixed_order, value_stream_id, event_count_stream_id) = plan
        .groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::PartitionRunLengths {
                group_id: candidate,
                fixed_order,
                value_stream_id,
                event_count_stream_id,
                ..
            } if *candidate == group_id => {
                Some((*fixed_order, *value_stream_id, *event_count_stream_id))
            }
            _ => None,
        })
        .ok_or(AuraError::InvalidValue("partition run reference"))?;
    let runs = partition_runs
        .get(&group_id)
        .ok_or(AuraError::InvalidValue("partition run reference"))?;
    let run_counts = if fixed_order {
        let order_len = stream_values
            .get(&value_stream_id)
            .ok_or(AuraError::InvalidValue("partition value stream"))?
            .len();
        if order_len == 0 {
            return Ok(Vec::new());
        }
        if runs.len() % order_len != 0 {
            return Err(AuraError::InvalidValue("partition run length"));
        }
        vec![order_len; runs.len() / order_len]
    } else {
        let event_count_stream_id =
            event_count_stream_id.ok_or(AuraError::InvalidValue("partition event count stream"))?;
        stream_values
            .get(&event_count_stream_id)
            .ok_or(AuraError::InvalidValue("partition event count stream"))?
            .iter()
            .map(|count| {
                if *count <= 0 {
                    return Err(AuraError::InvalidValue("partition event count stream"));
                }
                usize::try_from(*count)
                    .map_err(|_| AuraError::InvalidValue("partition event count stream"))
            })
            .collect::<Result<Vec<_>>>()?
    };
    if run_counts.iter().sum::<usize>() != runs.len() {
        return Err(AuraError::InvalidValue("partition event count stream"));
    }
    let mut run_index = 0usize;
    run_counts
        .into_iter()
        .map(|chunk| {
            let end_index = run_index
                .checked_add(chunk)
                .ok_or(AuraError::InvalidValue("partition run length"))?;
            if end_index > runs.len() {
                return Err(AuraError::InvalidValue("partition run length"));
            }
            let chunk = &runs[run_index..end_index];
            run_index = end_index;
            let first = chunk
                .first()
                .ok_or(AuraError::InvalidValue("partition run length"))?;
            let last = chunk
                .last()
                .ok_or(AuraError::InvalidValue("partition run length"))?;
            for pair in chunk.windows(2) {
                if pair[0].end != pair[1].start {
                    return Err(AuraError::InvalidValue("partition run length"));
                }
            }
            Ok((first.start, last.end))
        })
        .collect()
}

fn plan_i64_rows(schema: &SchemaDescriptor, rows: &[Vec<i64>]) -> Result<PlannedI64Rows> {
    validate_rows(schema, rows)?;
    let mut state = PlannerState::new();
    let partition_runs = add_group_hints(schema, rows, &mut state)?;
    add_group_value_hints(schema, rows, &mut state, partition_runs.as_ref())?;
    add_segmented_delta_hints(schema, rows, &mut state, partition_runs.as_ref())?;
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
) -> Result<Option<PartitionRunPlan>> {
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
        return Ok(None);
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
    let parent_group_id = group_id;

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

    let Some(candidate) = partition_run_candidate(schema, rows, &event_slots, &repeated_slots)?
    else {
        return Ok(None);
    };
    let value_stream_values = if let Some(values) = &candidate.fixed_order_values {
        values.clone()
    } else {
        candidate.runs.iter().map(|run| run.value).collect()
    };
    let count_stream_values = candidate
        .runs
        .iter()
        .map(|run| {
            i64::try_from(run.end - run.start).map_err(|_| AuraError::InvalidValue("run length"))
        })
        .collect::<Result<Vec<_>>>()?;
    let value_stream_id = state.add_stream(None, value_stream_values)?;
    let count_stream_id = state.add_stream(None, count_stream_values)?;
    let event_count_stream_id = if candidate.fixed_order_values.is_none() {
        Some(state.add_stream(None, candidate.event_run_counts.clone())?)
    } else {
        None
    };
    let group_id = state.next_group_id;
    state.next_group_id = state
        .next_group_id
        .checked_add(1)
        .ok_or(AuraError::InvalidValue("group id"))?;
    let fixed_order = candidate.fixed_order_values.is_some();
    state
        .groups
        .push(GenericGroupInstruction::PartitionRunLengths {
            group_id,
            parent_group_id,
            partition_slot: candidate.partition_slot,
            fixed_order,
            value_stream_id,
            count_stream_id,
            event_count_stream_id,
        });
    state.planned_slots.insert(candidate.partition_slot);

    Ok(Some(PartitionRunPlan {
        group_id,
        partition_slot: candidate.partition_slot,
        fixed_order_len: candidate.fixed_order_values.as_ref().map(Vec::len),
        has_event_run_counts: event_count_stream_id.is_some(),
        runs: candidate.runs,
    }))
}

fn add_group_value_hints(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    state: &mut PlannerState,
    partition_run_plan: Option<&PartitionRunPlan>,
) -> Result<()> {
    let Some(partition_run_plan) = partition_run_plan else {
        return Ok(());
    };
    if !partition_run_plan.has_event_run_counts
        && !partition_run_plan
            .fixed_order_len
            .is_some_and(|len| len > 0 && partition_run_plan.runs.len() % len == 0)
    {
        return Ok(());
    }
    let event_slots = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .map(|field| field.index)
        .collect::<Vec<_>>();
    let event_ranges = event_group_ranges(rows, &event_slots)?;
    if event_ranges.is_empty() {
        return Ok(());
    }
    for field in &schema.fields {
        if field.scope != FieldScope::Event || state.planned_slots.contains(&field.index) {
            continue;
        }
        let values = event_ranges
            .iter()
            .map(|(start, _)| rows[*start][usize::from(field.index)])
            .collect::<Vec<_>>();
        let direct_values = column_values(rows, field.index)?;
        if encoded_i64_len(&values)? >= encoded_i64_len(&direct_values)? {
            continue;
        }
        let stream_id = state.add_stream(None, values)?;
        let group_id = state.next_group_id;
        state.next_group_id = state
            .next_group_id
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("group id"))?;
        state
            .groups
            .push(GenericGroupInstruction::GroupValueStream {
                group_id,
                parent_group_id: partition_run_plan.group_id,
                output_slot: field.index,
                stream_id,
            });
        state.planned_slots.insert(field.index);
    }
    Ok(())
}

fn add_segmented_delta_hints(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    state: &mut PlannerState,
    partition_run_plan: Option<&PartitionRunPlan>,
) -> Result<()> {
    let Some(partition_run_plan) = partition_run_plan else {
        return Ok(());
    };
    for field in &schema.fields {
        if field.scope != FieldScope::Repeated || state.planned_slots.contains(&field.index) {
            continue;
        }
        if field.relation.related_field_index() != Some(partition_run_plan.partition_slot) {
            continue;
        }
        let Some(candidate) =
            segmented_delta_candidate(rows, &partition_run_plan.runs, field.index)?
        else {
            continue;
        };
        let base_stream_id = candidate
            .base_values
            .map(|values| state.add_stream(None, values))
            .transpose()?;
        let first_stream_id = state.add_stream(None, candidate.first_values)?;
        let delta_stream_id = state.add_stream(None, candidate.delta_values)?;
        let group_id = state.next_group_id;
        state.next_group_id = state
            .next_group_id
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("group id"))?;
        state
            .groups
            .push(GenericGroupInstruction::SegmentedDeltaStream {
                group_id,
                parent_group_id: partition_run_plan.group_id,
                output_slot: candidate.output_slot,
                base_stream_id,
                first_stream_id,
                delta_stream_id,
            });
        state.planned_slots.insert(candidate.output_slot);
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

fn partition_run_candidate(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    event_slots: &[u16],
    repeated_slots: &[u16],
) -> Result<Option<PartitionRunCandidate>> {
    if rows.is_empty() {
        return Ok(None);
    }
    let mut best = None;
    for partition_slot in repeated_slots {
        let child_slots = schema
            .fields
            .iter()
            .filter(|field| field.scope == FieldScope::Repeated)
            .filter(|field| field.relation.related_field_index() == Some(*partition_slot))
            .map(|field| field.index)
            .collect::<Vec<_>>();
        if child_slots.is_empty() {
            continue;
        }
        let runs = partition_run_ranges(rows, event_slots, *partition_slot)?;
        if runs.len() >= rows.len() {
            continue;
        }
        let mut unique_values = runs.iter().map(|run| run.value).collect::<Vec<_>>();
        unique_values.sort_unstable();
        unique_values.dedup();
        if unique_values.len() < 2 || unique_values.len() > 16 {
            continue;
        }
        if !child_slots.iter().any(|slot| {
            segmented_delta_candidate(rows, &runs, *slot)
                .map(|candidate| candidate.is_some())
                .unwrap_or(false)
        }) {
            continue;
        }
        let fixed_order_values = fixed_partition_run_order(rows, event_slots, *partition_slot)?;
        let event_run_counts = partition_run_event_counts(rows, event_slots, *partition_slot)?;
        let score = rows.len().saturating_sub(runs.len())
            + fixed_order_values
                .as_ref()
                .map_or(0, |values| runs.len().saturating_sub(values.len()));
        let candidate = PartitionRunCandidate {
            partition_slot: *partition_slot,
            fixed_order_values,
            event_run_counts,
            runs,
        };
        if best
            .as_ref()
            .is_none_or(|(best_score, _): &(usize, PartitionRunCandidate)| score > *best_score)
        {
            best = Some((score, candidate));
        }
    }
    Ok(best.map(|(_, candidate)| candidate))
}

fn segmented_delta_candidate(
    rows: &[Vec<i64>],
    runs: &[PartitionRun],
    output_slot: u16,
) -> Result<Option<SegmentedDeltaCandidate>> {
    if rows.is_empty() || runs.is_empty() {
        return Ok(None);
    }
    let values = column_values(rows, output_slot)?;
    let mut first_values = Vec::with_capacity(runs.len());
    let mut delta_values = Vec::with_capacity(rows.len().saturating_sub(runs.len()));
    let output_slot_index = usize::from(output_slot);
    for run in runs {
        if run.start >= run.end || run.end > rows.len() {
            return Err(AuraError::InvalidValue("partition run"));
        }
        first_values.push(rows[run.start][output_slot_index]);
        for row_index in run.start + 1..run.end {
            delta_values.push(checked_delta(
                rows[row_index][output_slot_index],
                rows[row_index - 1][output_slot_index],
            )?);
        }
    }
    let absolute_bytes = encoded_i64_len(&first_values)? + encoded_i64_len(&delta_values)?;
    let bases = partition_value_bases(rows, runs, output_slot)?;
    let base_values = bases.values().copied().collect::<Vec<_>>();
    let residual_first_values = runs
        .iter()
        .map(|run| {
            let base = bases
                .get(&run.value)
                .copied()
                .ok_or(AuraError::InvalidValue("partition base"))?;
            checked_delta(rows[run.start][output_slot_index], base)
        })
        .collect::<Result<Vec<_>>>()?;
    let partition_base_bytes = encoded_i64_len(&base_values)?
        + encoded_i64_len(&residual_first_values)?
        + encoded_i64_len(&delta_values)?;
    let (base_values, first_values, estimated_bytes) = if partition_base_bytes < absolute_bytes {
        (
            Some(base_values),
            residual_first_values,
            partition_base_bytes,
        )
    } else {
        (None, first_values, absolute_bytes)
    };
    if estimated_bytes >= encoded_i64_len(&values)? {
        return Ok(None);
    }
    Ok(Some(SegmentedDeltaCandidate {
        output_slot,
        base_values,
        first_values,
        delta_values,
    }))
}

fn partition_value_bases(
    rows: &[Vec<i64>],
    runs: &[PartitionRun],
    output_slot: u16,
) -> Result<BTreeMap<i64, i64>> {
    let output_slot = usize::from(output_slot);
    let mut bases: BTreeMap<i64, i64> = BTreeMap::new();
    for run in runs {
        if run.start >= run.end || run.end > rows.len() {
            return Err(AuraError::InvalidValue("partition run"));
        }
        for row in rows.iter().take(run.end).skip(run.start) {
            let value = row[output_slot];
            bases
                .entry(run.value)
                .and_modify(|base| *base = (*base).min(value))
                .or_insert(value);
        }
    }
    Ok(bases)
}

fn partition_run_ranges(
    rows: &[Vec<i64>],
    event_slots: &[u16],
    partition_slot: u16,
) -> Result<Vec<PartitionRun>> {
    let mut runs = Vec::new();
    let partition_slot = usize::from(partition_slot);
    for (group_start, group_end) in event_group_ranges(rows, event_slots)? {
        if group_start >= group_end {
            continue;
        }
        let mut run_start = group_start;
        let mut value = *rows[group_start]
            .get(partition_slot)
            .ok_or(AuraError::InvalidValue("partition slot"))?;
        for (row_index, row) in rows
            .iter()
            .enumerate()
            .take(group_end)
            .skip(group_start + 1)
        {
            let next = *row
                .get(partition_slot)
                .ok_or(AuraError::InvalidValue("partition slot"))?;
            if next != value {
                runs.push(PartitionRun {
                    start: run_start,
                    end: row_index,
                    value,
                });
                run_start = row_index;
                value = next;
            }
        }
        runs.push(PartitionRun {
            start: run_start,
            end: group_end,
            value,
        });
    }
    Ok(runs)
}

fn partition_run_event_counts(
    rows: &[Vec<i64>],
    event_slots: &[u16],
    partition_slot: u16,
) -> Result<Vec<i64>> {
    let partition_slot = usize::from(partition_slot);
    event_group_ranges(rows, event_slots)?
        .into_iter()
        .map(|(group_start, group_end)| {
            if group_start >= group_end {
                return Ok(0);
            }
            let mut count = 1usize;
            let mut value = rows[group_start][partition_slot];
            for row in rows.iter().take(group_end).skip(group_start + 1) {
                let next = row[partition_slot];
                if next != value {
                    value = next;
                    count += 1;
                }
            }
            i64::try_from(count).map_err(|_| AuraError::InvalidValue("partition event count"))
        })
        .collect()
}

fn fixed_partition_run_order(
    rows: &[Vec<i64>],
    event_slots: &[u16],
    partition_slot: u16,
) -> Result<Option<Vec<i64>>> {
    let groups = event_group_ranges(rows, event_slots)?;
    if groups.len() < 2 {
        return Ok(None);
    }
    let partition_slot = usize::from(partition_slot);
    let mut fixed_order: Option<Vec<i64>> = None;
    for (group_start, group_end) in groups {
        let mut order = Vec::new();
        if group_start >= group_end {
            continue;
        }
        let mut value = rows[group_start][partition_slot];
        order.push(value);
        for row in rows.iter().take(group_end).skip(group_start + 1) {
            let next = row[partition_slot];
            if next != value {
                value = next;
                order.push(value);
            }
        }
        if order.is_empty() {
            return Ok(None);
        }
        if let Some(existing) = &fixed_order {
            if existing != &order {
                return Ok(None);
            }
        } else {
            fixed_order = Some(order);
        }
    }
    Ok(fixed_order)
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
