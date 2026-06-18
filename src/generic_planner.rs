use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::time::Instant;

use crate::bitpack::{signed_bitpack_width_for_range, unsigned_bitpack_width};
use crate::body::{
    decode_generic_stream_body, encode_generic_stream_body, try_generic_i64_stream_cursor,
    GenericI64StreamCursor, GenericStreamBodyValue,
};
use crate::bytes::{put_u16_le, put_u32_le, put_u64_le, ByteReader};
use crate::header::{DerivedExpression, DerivedExpressionOp};
use crate::instructions::{
    DerivedOp, GenericGroupInstruction, GenericInstructionPlan, GenericStreamInstruction,
    GenericStreamOp,
};
use crate::plan::Aura1Plan;
use crate::schema::{FieldRelation, FieldScope, SchemaDescriptor};
use crate::{AuraError, PhysicalWidth, Result};

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

const MAX_STREAMING_AURA1_FIELDS: usize = 64;

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

enum SlotPlanCandidate {
    Direct {
        values: Vec<i64>,
        score: usize,
    },
    Derived {
        op: DerivedOp,
        input_slots: Vec<u16>,
        values: Vec<i64>,
        score: usize,
    },
    Expression {
        op: DerivedExpressionOp,
        input_slots: Vec<u16>,
        literals: Vec<i64>,
        values: Vec<i64>,
        score: usize,
    },
    ExpressionValue {
        op: DerivedExpressionOp,
        input_slots: Vec<u16>,
        literals: Vec<i64>,
        residual: i64,
        score: usize,
    },
}

impl SlotPlanCandidate {
    const fn score(&self) -> usize {
        match self {
            Self::Direct { score, .. }
            | Self::Derived { score, .. }
            | Self::Expression { score, .. }
            | Self::ExpressionValue { score, .. } => *score,
        }
    }
}

enum PendingDerivedInstruction<'a> {
    Residual {
        output_slot: u16,
        op: DerivedOp,
        input_slots: &'a [u16],
        stream_id: u16,
    },
    Expression {
        output_slot: u16,
        op: DerivedExpressionOp,
        input_slots: &'a [u16],
        literals: &'a [i64],
        stream_id: u16,
    },
    ExpressionValue {
        output_slot: u16,
        op: DerivedExpressionOp,
        input_slots: &'a [u16],
        literals: &'a [i64],
        residual: i64,
    },
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

    fn add_expression(
        &mut self,
        output_slot: u16,
        op: DerivedExpressionOp,
        input_slots: Vec<u16>,
        literals: Vec<i64>,
        values: Vec<i64>,
    ) -> Result<()> {
        let stream_id = self.add_stream(None, values)?;
        let group_id = self.next_group_id;
        self.next_group_id = self
            .next_group_id
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("group id"))?;
        self.groups.push(GenericGroupInstruction::ExpressionStream {
            group_id,
            parent_group_id: None,
            output_slot,
            op,
            input_slots,
            literals,
            stream_id,
        });
        self.planned_slots.insert(output_slot);
        Ok(())
    }

    fn add_expression_value(
        &mut self,
        output_slot: u16,
        op: DerivedExpressionOp,
        input_slots: Vec<u16>,
        literals: Vec<i64>,
        residual: i64,
    ) -> Result<()> {
        let group_id = self.next_group_id;
        self.next_group_id = self
            .next_group_id
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("group id"))?;
        self.groups.push(GenericGroupInstruction::ExpressionValue {
            group_id,
            parent_group_id: None,
            output_slot,
            op,
            input_slots,
            literals,
            residual,
        });
        self.planned_slots.insert(output_slot);
        Ok(())
    }

    fn add_slot_candidate(&mut self, output_slot: u16, candidate: SlotPlanCandidate) -> Result<()> {
        match candidate {
            SlotPlanCandidate::Direct { values, .. } => {
                self.add_stream(Some(output_slot), values)?;
                self.planned_slots.insert(output_slot);
                Ok(())
            }
            SlotPlanCandidate::Derived {
                op,
                input_slots,
                values,
                ..
            } => self.add_derived(output_slot, op, input_slots, values),
            SlotPlanCandidate::Expression {
                op,
                input_slots,
                literals,
                values,
                ..
            } => self.add_expression(output_slot, op, input_slots, literals, values),
            SlotPlanCandidate::ExpressionValue {
                op,
                input_slots,
                literals,
                residual,
                ..
            } => self.add_expression_value(output_slot, op, input_slots, literals, residual),
        }
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

pub(crate) fn try_decode_generic_i64_columns_body(
    plan: GenericInstructionPlan,
    bytes: &[u8],
    record_count: usize,
    field_count: usize,
) -> Result<Option<Vec<Vec<i64>>>> {
    let profile = std::env::var_os("AURA_PROFILE_FAST").is_some();
    if plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PartitionRuns { .. }
                | GenericGroupInstruction::DerivedStream { .. }
                | GenericGroupInstruction::ExpressionStream { .. }
                | GenericGroupInstruction::ExpressionValue { .. }
        )
    }) {
        return Ok(None);
    }

    let instructions = plan
        .streams
        .iter()
        .map(|instruction| (instruction.stream_id, instruction))
        .collect::<BTreeMap<_, _>>();
    let mut stream_values = BTreeMap::new();
    let mut reader = ByteReader::new(bytes);
    let stream_count = reader.read_u16_le()? as usize;
    for _ in 0..stream_count {
        let stream_id = reader.read_u16_le()?;
        let value_count = usize::try_from(reader.read_u64_le()?)
            .map_err(|_| AuraError::InvalidValue("stream value count"))?;
        let body_len = reader.read_u32_le()? as usize;
        let body = reader.read_exact(body_len)?;
        let instruction = instructions
            .get(&stream_id)
            .ok_or(AuraError::InvalidValue("stream id"))?;
        let stage_start = Instant::now();
        match decode_generic_stream_body(instruction, body, value_count)? {
            GenericStreamBodyValue::I64(values) => {
                if profile {
                    eprintln!(
                        "generic stream id={} values={} op={} decode_us={}",
                        stream_id,
                        value_count,
                        generic_op_name(&instruction.op),
                        stage_start.elapsed().as_micros()
                    );
                }
                stream_values.insert(stream_id, values);
            }
            GenericStreamBodyValue::U128(_) => return Err(AuraError::InvalidValue("body type")),
        }
    }
    reader.finish()?;

    let stage_start = Instant::now();
    let columns =
        materialize_generic_i64_columns(&plan, &stream_values, record_count, field_count)?;
    if profile {
        eprintln!(
            "generic materialize_us={}",
            stage_start.elapsed().as_micros()
        );
    }
    Ok(columns)
}

pub(crate) fn try_encode_generic_i64_aura1_body(
    plan: GenericInstructionPlan,
    bytes: &[u8],
    record_count: usize,
    field_count: usize,
    aura1_plan: &Aura1Plan,
) -> Result<Option<Vec<u8>>> {
    if plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PartitionRuns { .. }
                | GenericGroupInstruction::DerivedStream { .. }
                | GenericGroupInstruction::ExpressionStream { .. }
                | GenericGroupInstruction::ExpressionValue { .. }
        )
    }) {
        return Ok(None);
    }

    let stream_values = decode_generic_i64_stream_values(&plan, bytes)?;
    let partition_runs =
        partition_run_lengths_from_streams(&plan, &stream_values, record_count, field_count)?;
    let presence_maps = presence_maps_by_group(&plan, &stream_values, record_count)?;
    let mut sources = direct_aura1_slot_sources(
        &plan,
        &stream_values,
        &partition_runs,
        &presence_maps,
        record_count,
        field_count,
    )?;

    let mut field_specs = Vec::with_capacity(aura1_plan.fields.len());
    let mut row_width = 0usize;
    for field_plan in &aura1_plan.fields {
        let slot = usize::from(field_plan.field_index);
        if slot >= field_count {
            return Err(AuraError::InvalidValue("field index"));
        }
        if !sources[slot].is_supported() {
            return Ok(None);
        }
        row_width = row_width
            .checked_add(usize::from(field_plan.width.byte_width()))
            .ok_or(AuraError::InvalidValue("body length"))?;
        field_specs.push((slot, field_plan.width));
    }

    let mut out = Vec::with_capacity(
        record_count
            .checked_mul(row_width)
            .ok_or(AuraError::InvalidValue("body length"))?,
    );
    for row_index in 0..record_count {
        for (slot, width) in &field_specs {
            let value = sources[*slot].value_at(row_index)?;
            write_direct_i64_width(&mut out, value, *width)?;
        }
    }
    for source in &mut sources {
        source.finish()?;
    }
    Ok(Some(out))
}

pub(crate) fn try_encode_generic_i64_aura1_body_streaming(
    plan: GenericInstructionPlan,
    bytes: &[u8],
    record_count: usize,
    field_count: usize,
    aura1_plan: &Aura1Plan,
) -> Result<Option<Vec<u8>>> {
    if field_count > MAX_STREAMING_AURA1_FIELDS || aura1_plan.fields.len() > MAX_STREAMING_AURA1_FIELDS
    {
        return Ok(None);
    }
    if plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PartitionRuns { .. }
                | GenericGroupInstruction::DerivedStream { .. }
                | GenericGroupInstruction::ExpressionStream { .. }
                | GenericGroupInstruction::ExpressionValue { .. }
        )
    }) {
        return Ok(None);
    }

    let instructions = plan
        .streams
        .iter()
        .map(|instruction| (instruction.stream_id, instruction))
        .collect::<BTreeMap<_, _>>();
    let mut cursors = BTreeMap::new();
    let mut reader = ByteReader::new(bytes);
    let stream_count = reader.read_u16_le()? as usize;
    for _ in 0..stream_count {
        let stream_id = reader.read_u16_le()?;
        let value_count = usize::try_from(reader.read_u64_le()?)
            .map_err(|_| AuraError::InvalidValue("stream value count"))?;
        let body_len = reader.read_u32_le()? as usize;
        let body = reader.read_exact(body_len)?;
        let instruction = instructions
            .get(&stream_id)
            .ok_or(AuraError::InvalidValue("stream id"))?;
        let Some(cursor) = try_generic_i64_stream_cursor(instruction, body, value_count)? else {
            return Ok(None);
        };
        cursors.insert(stream_id, cursor);
    }
    reader.finish()?;

    let Some(config) = StreamingAura1Config::from_plan(&plan, field_count)? else {
        return Ok(None);
    };
    let mut segment_bases = [(0i64, 0i64); MAX_STREAMING_AURA1_FIELDS];
    let mut segment_base_count = 0usize;
    if let Some(segmented) = config.segmented {
        if let Some(base_stream_id) = segmented.base_stream_id {
            let mut partition_values = [0i64; MAX_STREAMING_AURA1_FIELDS];
            let partition_value_count = {
                let Some(values) = cursors
                    .get(&config.partition.value_stream_id)
                    .and_then(GenericI64StreamCursor::dictionary_values)
                else {
                    return Ok(None);
                };
                if values.len() > partition_values.len() {
                    return Ok(None);
                }
                for (index, value) in values.iter().copied().enumerate() {
                    partition_values[index] = value;
                }
                values.len()
            };
            for partition_value in partition_values
                .iter()
                .copied()
                .take(partition_value_count)
            {
                let base = next_stream_value(&mut cursors, base_stream_id, "segmented base stream")?;
                segment_bases[segment_base_count] = (partition_value, base);
                segment_base_count += 1;
            }
        }
    }

    let mut direct_slots = [0usize; MAX_STREAMING_AURA1_FIELDS];
    let mut direct_slot_count = 0usize;
    let mut group_value_slots = [0usize; MAX_STREAMING_AURA1_FIELDS];
    let mut group_value_slot_count = 0usize;
    let mut sparse_slots = [0usize; MAX_STREAMING_AURA1_FIELDS];
    let mut sparse_slot_count = 0usize;
    let mut presence_value_slots = [0usize; MAX_STREAMING_AURA1_FIELDS];
    let mut presence_value_slot_count = 0usize;
    for slot in 0..field_count {
        if config.direct_streams[slot].is_some() {
            direct_slots[direct_slot_count] = slot;
            direct_slot_count += 1;
        }
        if config.group_value_streams[slot].is_some() {
            group_value_slots[group_value_slot_count] = slot;
            group_value_slot_count += 1;
        }
        if config.sparse_streams[slot].is_some() {
            sparse_slots[sparse_slot_count] = slot;
            sparse_slot_count += 1;
        }
        if config.presence_values[slot].is_some() {
            presence_value_slots[presence_value_slot_count] = slot;
            presence_value_slot_count += 1;
        }
    }

    let mut direct_cursors: [Option<GenericI64StreamCursor<'_>>; MAX_STREAMING_AURA1_FIELDS] =
        std::array::from_fn(|_| None);
    for slot in direct_slots.iter().copied().take(direct_slot_count) {
        let stream_id = config.direct_streams[slot].ok_or(AuraError::InvalidValue("stream body"))?;
        direct_cursors[slot] = Some(take_stream_cursor(&mut cursors, stream_id, "stream body")?);
    }
    let mut group_value_cursors: [Option<GenericI64StreamCursor<'_>>; MAX_STREAMING_AURA1_FIELDS] =
        std::array::from_fn(|_| None);
    for slot in group_value_slots.iter().copied().take(group_value_slot_count) {
        let stream_id = config.group_value_streams[slot]
            .ok_or(AuraError::InvalidValue("group value stream"))?;
        group_value_cursors[slot] =
            Some(take_stream_cursor(&mut cursors, stream_id, "group value stream")?);
    }
    let mut sparse_cursors: [Option<GenericI64StreamCursor<'_>>; MAX_STREAMING_AURA1_FIELDS] =
        std::array::from_fn(|_| None);
    for slot in sparse_slots.iter().copied().take(sparse_slot_count) {
        let sparse = config.sparse_streams[slot].ok_or(AuraError::InvalidValue("sparse stream body"))?;
        sparse_cursors[usize::from(sparse.output_slot)] =
            Some(take_stream_cursor(&mut cursors, sparse.stream_id, "sparse stream body")?);
    }
    let mut partition_value_cursor = take_stream_cursor(
        &mut cursors,
        config.partition.value_stream_id,
        "partition value stream",
    )?;
    let mut partition_count_cursor =
        take_stream_cursor(&mut cursors, config.partition.count_stream_id, "partition run length")?;
    let mut partition_event_count_cursor = match config.partition.event_count_stream_id {
        Some(stream_id) => Some(take_stream_cursor(
            &mut cursors,
            stream_id,
            "partition event count stream",
        )?),
        None => None,
    };
    let mut segmented_first_cursor = match config.segmented {
        Some(segmented) => Some(take_stream_cursor(
            &mut cursors,
            segmented.first_stream_id,
            "segmented first stream",
        )?),
        None => None,
    };
    let mut segmented_delta_cursor = match config.segmented {
        Some(segmented) => Some(take_stream_cursor(
            &mut cursors,
            segmented.delta_stream_id,
            "segmented delta stream",
        )?),
        None => None,
    };
    let mut presence_cursor = match config.presence {
        Some(presence) => Some(take_stream_cursor(
            &mut cursors,
            presence.stream_id,
            "presence stream body",
        )?),
        None => None,
    };

    let mut field_specs = [None; MAX_STREAMING_AURA1_FIELDS];
    let mut field_spec_count = 0usize;
    let mut row_width = 0usize;
    for field_plan in &aura1_plan.fields {
        let slot = usize::from(field_plan.field_index);
        if slot >= field_count || !config.source_supported(slot) {
            return Ok(None);
        }
        row_width = row_width
            .checked_add(usize::from(field_plan.width.byte_width()))
            .ok_or(AuraError::InvalidValue("body length"))?;
        field_specs[field_spec_count] = Some((slot, field_plan.width));
        field_spec_count += 1;
    }

    let mut out = Vec::with_capacity(
        record_count
            .checked_mul(row_width)
            .ok_or(AuraError::InvalidValue("body length"))?,
    );
    let mut event_values = [0i64; MAX_STREAMING_AURA1_FIELDS];
    let mut event_active = false;
    let mut event_runs_remaining = 0usize;
    let mut run_active = false;
    let mut run_start = 0usize;
    let mut run_end = 0usize;
    let mut partition_value = 0i64;
    let mut segmented_current = 0i64;
    let mut segmented_initialized = false;

    for row_index in 0..record_count {
        if !event_active {
            let event_count_cursor = partition_event_count_cursor
                .as_mut()
                .ok_or(AuraError::InvalidValue("partition event count stream"))?;
            event_runs_remaining =
                next_positive_cursor_value(event_count_cursor, "partition event count stream")?;
            for slot in group_value_slots.iter().copied().take(group_value_slot_count) {
                let cursor = group_value_cursors[slot]
                    .as_mut()
                    .ok_or(AuraError::InvalidValue("group value stream"))?;
                event_values[slot] = next_cursor_value(cursor, "group value stream")?;
            }
            event_active = true;
        }

        if !run_active {
            if event_runs_remaining == 0 {
                return Err(AuraError::InvalidValue("partition event count stream"));
            }
            partition_value = next_cursor_value(&mut partition_value_cursor, "partition value stream")?;
            let count =
                next_positive_cursor_value(&mut partition_count_cursor, "partition run length")?;
            run_start = row_index;
            run_end = row_index
                .checked_add(count)
                .ok_or(AuraError::InvalidValue("partition run length"))?;
            if run_end > record_count {
                return Err(AuraError::InvalidValue("partition run length"));
            }
            event_runs_remaining -= 1;
            run_active = true;

            if let Some(segmented) = config.segmented {
                let first = next_cursor_value(
                    segmented_first_cursor
                        .as_mut()
                        .ok_or(AuraError::InvalidValue("segmented first stream"))?,
                    "segmented first stream",
                )?;
                segmented_current = if segmented.base_stream_id.is_some() {
                    checked_sum(
                        lookup_segment_base(&segment_bases, segment_base_count, partition_value)?,
                        first,
                    )?
                } else {
                    first
                };
                segmented_initialized = true;
            }
        }

        let mut row_values = [0i64; MAX_STREAMING_AURA1_FIELDS];
        for slot in direct_slots.iter().copied().take(direct_slot_count) {
            let cursor = direct_cursors[slot]
                .as_mut()
                .ok_or(AuraError::InvalidValue("stream body"))?;
            row_values[slot] = next_cursor_value(cursor, "stream body")?;
        }
        for slot in group_value_slots.iter().copied().take(group_value_slot_count) {
            row_values[slot] = event_values[slot];
        }
        row_values[usize::from(config.partition.partition_slot)] = partition_value;
        if let Some(segmented) = config.segmented {
            if row_index != run_start {
                if !segmented_initialized {
                    return Err(AuraError::InvalidValue("segmented delta stream"));
                }
                let delta = next_cursor_value(
                    segmented_delta_cursor
                        .as_mut()
                        .ok_or(AuraError::InvalidValue("segmented delta stream"))?,
                    "segmented delta stream",
                )?;
                segmented_current = checked_sum(segmented_current, delta)?;
            }
            row_values[usize::from(segmented.output_slot)] = segmented_current;
        }

        if let Some(presence) = config.presence {
            let mask = next_cursor_value(
                presence_cursor
                    .as_mut()
                    .ok_or(AuraError::InvalidValue("presence stream body"))?,
                "presence stream body",
            )?;
            if mask < 0 {
                return Err(AuraError::InvalidValue("presence bit"));
            }
            for slot in sparse_slots.iter().copied().take(sparse_slot_count) {
                let sparse =
                    config.sparse_streams[slot].ok_or(AuraError::InvalidValue("sparse stream body"))?;
                if sparse.presence_group_id != presence.group_id {
                    return Ok(None);
                }
                let bit = presence_bit_mask(sparse.presence_index)?;
                row_values[usize::from(sparse.output_slot)] = if mask & bit == 0 {
                    0
                } else {
                    next_cursor_value(
                        sparse_cursors[usize::from(sparse.output_slot)]
                            .as_mut()
                            .ok_or(AuraError::InvalidValue("sparse stream body"))?,
                        "sparse stream body",
                    )?
                };
            }
            for slot in presence_value_slots
                .iter()
                .copied()
                .take(presence_value_slot_count)
            {
                let value =
                    config.presence_values[slot].ok_or(AuraError::InvalidValue("presence bit"))?;
                if value.presence_group_id != presence.group_id {
                    return Ok(None);
                }
                let bit = presence_bit_mask(value.presence_index)?;
                row_values[usize::from(value.output_slot)] = if mask & bit != 0 {
                    value.value
                } else {
                    0
                };
            }
        }

        for (slot, width) in field_specs.iter().flatten().copied().take(field_spec_count) {
            write_direct_i64_width(&mut out, row_values[slot], width)?;
        }

        if row_index + 1 == run_end {
            run_active = false;
            segmented_initialized = false;
            if event_runs_remaining == 0 {
                event_active = false;
            }
        }
    }

    if run_active || event_active || event_runs_remaining != 0 {
        return Err(AuraError::InvalidValue("partition run length"));
    }
    partition_value_cursor.finish()?;
    partition_count_cursor.finish()?;
    if let Some(cursor) = &mut partition_event_count_cursor {
        cursor.finish()?;
    }
    if let Some(cursor) = &mut segmented_first_cursor {
        cursor.finish()?;
    }
    if let Some(cursor) = &mut segmented_delta_cursor {
        cursor.finish()?;
    }
    if let Some(cursor) = &mut presence_cursor {
        cursor.finish()?;
    }
    for cursor in direct_cursors.iter_mut().flatten() {
        cursor.finish()?;
    }
    for cursor in group_value_cursors.iter_mut().flatten() {
        cursor.finish()?;
    }
    for cursor in sparse_cursors.iter_mut().flatten() {
        cursor.finish()?;
    }
    for cursor in cursors.values_mut() {
        cursor.finish()?;
    }
    Ok(Some(out))
}

#[derive(Clone, Copy)]
struct StreamingPartitionConfig {
    partition_slot: u16,
    value_stream_id: u16,
    count_stream_id: u16,
    event_count_stream_id: Option<u16>,
}

#[derive(Clone, Copy)]
struct StreamingSegmentedConfig {
    output_slot: u16,
    base_stream_id: Option<u16>,
    first_stream_id: u16,
    delta_stream_id: u16,
}

#[derive(Clone, Copy)]
struct StreamingPresenceConfig {
    group_id: u16,
    stream_id: u16,
}

#[derive(Clone, Copy)]
struct StreamingSparseConfig {
    presence_group_id: u16,
    output_slot: u16,
    presence_index: u16,
    stream_id: u16,
}

#[derive(Clone, Copy)]
struct StreamingPresenceValueConfig {
    presence_group_id: u16,
    output_slot: u16,
    presence_index: u16,
    value: i64,
}

struct StreamingAura1Config {
    partition: StreamingPartitionConfig,
    direct_streams: [Option<u16>; MAX_STREAMING_AURA1_FIELDS],
    group_value_streams: [Option<u16>; MAX_STREAMING_AURA1_FIELDS],
    segmented: Option<StreamingSegmentedConfig>,
    presence: Option<StreamingPresenceConfig>,
    sparse_streams: [Option<StreamingSparseConfig>; MAX_STREAMING_AURA1_FIELDS],
    presence_values: [Option<StreamingPresenceValueConfig>; MAX_STREAMING_AURA1_FIELDS],
}

impl StreamingAura1Config {
    fn from_plan(plan: &GenericInstructionPlan, field_count: usize) -> Result<Option<Self>> {
        let mut config = Self {
            partition: StreamingPartitionConfig {
                partition_slot: 0,
                value_stream_id: 0,
                count_stream_id: 0,
                event_count_stream_id: None,
            },
            direct_streams: [None; MAX_STREAMING_AURA1_FIELDS],
            group_value_streams: [None; MAX_STREAMING_AURA1_FIELDS],
            segmented: None,
            presence: None,
            sparse_streams: [None; MAX_STREAMING_AURA1_FIELDS],
            presence_values: [None; MAX_STREAMING_AURA1_FIELDS],
        };
        let mut has_partition = false;

        for instruction in &plan.streams {
            let Some(slot) = instruction.target_slot else {
                continue;
            };
            let slot = usize::from(slot);
            if slot >= field_count || slot >= MAX_STREAMING_AURA1_FIELDS {
                return Err(AuraError::InvalidValue("target slot"));
            }
            config.direct_streams[slot] = Some(instruction.stream_id);
        }

        for group in &plan.groups {
            match group {
                GenericGroupInstruction::Group { .. } => {}
                GenericGroupInstruction::PartitionRunLengths {
                    partition_slot,
                    fixed_order,
                    value_stream_id,
                    count_stream_id,
                    event_count_stream_id,
                    ..
                } => {
                    if has_partition || *fixed_order {
                        return Ok(None);
                    }
                    let slot = usize::from(*partition_slot);
                    if slot >= field_count || slot >= MAX_STREAMING_AURA1_FIELDS {
                        return Err(AuraError::InvalidValue("partition slot"));
                    }
                    config.partition = StreamingPartitionConfig {
                        partition_slot: *partition_slot,
                        value_stream_id: *value_stream_id,
                        count_stream_id: *count_stream_id,
                        event_count_stream_id: *event_count_stream_id,
                    };
                    has_partition = true;
                }
                GenericGroupInstruction::GroupValueStream {
                    output_slot,
                    stream_id,
                    ..
                } => {
                    let slot = usize::from(*output_slot);
                    if slot >= field_count || slot >= MAX_STREAMING_AURA1_FIELDS {
                        return Err(AuraError::InvalidValue("target slot"));
                    }
                    config.group_value_streams[slot] = Some(*stream_id);
                }
                GenericGroupInstruction::SegmentedDeltaStream {
                    output_slot,
                    base_stream_id,
                    first_stream_id,
                    delta_stream_id,
                    ..
                } => {
                    if config.segmented.is_some() {
                        return Ok(None);
                    }
                    let slot = usize::from(*output_slot);
                    if slot >= field_count || slot >= MAX_STREAMING_AURA1_FIELDS {
                        return Err(AuraError::InvalidValue("target slot"));
                    }
                    config.segmented = Some(StreamingSegmentedConfig {
                        output_slot: *output_slot,
                        base_stream_id: *base_stream_id,
                        first_stream_id: *first_stream_id,
                        delta_stream_id: *delta_stream_id,
                    });
                }
                GenericGroupInstruction::PresenceMap {
                    group_id,
                    stream_id,
                    ..
                } => {
                    if config.presence.is_some() {
                        return Ok(None);
                    }
                    config.presence = Some(StreamingPresenceConfig {
                        group_id: *group_id,
                        stream_id: *stream_id,
                    });
                }
                GenericGroupInstruction::SparseStream {
                    presence_group_id,
                    output_slot,
                    presence_index,
                    stream_id,
                    ..
                } => {
                    let slot = usize::from(*output_slot);
                    if slot >= field_count || slot >= MAX_STREAMING_AURA1_FIELDS {
                        return Err(AuraError::InvalidValue("target slot"));
                    }
                    config.sparse_streams[slot] = Some(StreamingSparseConfig {
                        presence_group_id: *presence_group_id,
                        output_slot: *output_slot,
                        presence_index: *presence_index,
                        stream_id: *stream_id,
                    });
                }
                GenericGroupInstruction::PresenceValue {
                    presence_group_id,
                    output_slot,
                    presence_index,
                    value,
                    ..
                } => {
                    let slot = usize::from(*output_slot);
                    if slot >= field_count || slot >= MAX_STREAMING_AURA1_FIELDS {
                        return Err(AuraError::InvalidValue("target slot"));
                    }
                    config.presence_values[slot] = Some(StreamingPresenceValueConfig {
                        presence_group_id: *presence_group_id,
                        output_slot: *output_slot,
                        presence_index: *presence_index,
                        value: *value,
                    });
                }
                GenericGroupInstruction::PartitionRuns { .. }
                | GenericGroupInstruction::DerivedStream { .. }
                | GenericGroupInstruction::ExpressionStream { .. }
                | GenericGroupInstruction::ExpressionValue { .. } => return Ok(None),
            }
        }

        if has_partition {
            Ok(Some(config))
        } else {
            Ok(None)
        }
    }

    fn source_supported(&self, slot: usize) -> bool {
        self.direct_streams[slot].is_some()
            || usize::from(self.partition.partition_slot) == slot
            || self.group_value_streams[slot].is_some()
            || self
                .segmented
                .is_some_and(|segmented| usize::from(segmented.output_slot) == slot)
            || self.sparse_streams[slot].is_some()
            || self.presence_values[slot].is_some()
    }
}

fn next_stream_value(
    cursors: &mut BTreeMap<u16, GenericI64StreamCursor<'_>>,
    stream_id: u16,
    name: &'static str,
) -> Result<i64> {
    cursors
        .get_mut(&stream_id)
        .ok_or(AuraError::InvalidValue(name))?
        .next_i64()
}

fn take_stream_cursor<'a>(
    cursors: &mut BTreeMap<u16, GenericI64StreamCursor<'a>>,
    stream_id: u16,
    name: &'static str,
) -> Result<GenericI64StreamCursor<'a>> {
    cursors
        .remove(&stream_id)
        .ok_or(AuraError::InvalidValue(name))
}

fn next_cursor_value(cursor: &mut GenericI64StreamCursor<'_>, name: &'static str) -> Result<i64> {
    cursor
        .next_i64()
        .map_err(|_| AuraError::InvalidValue(name))
}

fn next_positive_cursor_value(
    cursor: &mut GenericI64StreamCursor<'_>,
    name: &'static str,
) -> Result<usize> {
    let value = next_cursor_value(cursor, name)?;
    if value <= 0 {
        return Err(AuraError::InvalidValue(name));
    }
    usize::try_from(value).map_err(|_| AuraError::InvalidValue(name))
}

fn lookup_segment_base(bases: &[(i64, i64)], count: usize, partition_value: i64) -> Result<i64> {
    bases
        .iter()
        .copied()
        .take(count)
        .find_map(|(value, base)| (value == partition_value).then_some(base))
        .ok_or(AuraError::InvalidValue("segmented base stream"))
}

fn generic_op_name(op: &GenericStreamOp) -> &'static str {
    match op {
        GenericStreamOp::FixedStep { .. } => "FixedStep",
        GenericStreamOp::BaseBitpack { .. } => "BaseBitpack",
        GenericStreamOp::PrevDelta { .. } => "PrevDelta",
        GenericStreamOp::PrevVarint { .. } => "PrevVarint",
        GenericStreamOp::BlockLocal { .. } => "BlockLocal",
        GenericStreamOp::PatchedBitpack { .. } => "PatchedBitpack",
        GenericStreamOp::Rle { .. } => "Rle",
        GenericStreamOp::BitplaneRle { .. } => "BitplaneRle",
        GenericStreamOp::Dictionary { .. } => "Dictionary",
        GenericStreamOp::PackedDictionary { .. } => "PackedDictionary",
        GenericStreamOp::HuffmanDictionary { .. } => "HuffmanDictionary",
        GenericStreamOp::UuidConstMask { .. } => "UuidConstMask",
    }
}

fn decode_generic_i64_stream_values(
    plan: &GenericInstructionPlan,
    bytes: &[u8],
) -> Result<BTreeMap<u16, Vec<i64>>> {
    let instructions = plan
        .streams
        .iter()
        .map(|instruction| (instruction.stream_id, instruction))
        .collect::<BTreeMap<_, _>>();
    let mut stream_values = BTreeMap::new();
    let mut reader = ByteReader::new(bytes);
    let stream_count = reader.read_u16_le()? as usize;
    for _ in 0..stream_count {
        let stream_id = reader.read_u16_le()?;
        let value_count = usize::try_from(reader.read_u64_le()?)
            .map_err(|_| AuraError::InvalidValue("stream value count"))?;
        let body_len = reader.read_u32_le()? as usize;
        let body = reader.read_exact(body_len)?;
        let instruction = instructions
            .get(&stream_id)
            .ok_or(AuraError::InvalidValue("stream id"))?;
        match decode_generic_stream_body(instruction, body, value_count)? {
            GenericStreamBodyValue::I64(values) => {
                stream_values.insert(stream_id, values);
            }
            GenericStreamBodyValue::U128(_) => return Err(AuraError::InvalidValue("body type")),
        }
    }
    reader.finish()?;
    Ok(stream_values)
}

fn partition_run_lengths_from_streams(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    record_count: usize,
    field_count: usize,
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
        if usize::from(*partition_slot) >= field_count {
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

fn direct_aura1_slot_sources<'a>(
    plan: &GenericInstructionPlan,
    stream_values: &'a BTreeMap<u16, Vec<i64>>,
    partition_runs: &'a BTreeMap<u16, Vec<PartitionRun>>,
    presence_maps: &BTreeMap<u16, &'a [i64]>,
    record_count: usize,
    field_count: usize,
) -> Result<Vec<DirectAura1SlotSource<'a>>> {
    let mut sources = (0..field_count)
        .map(|_| DirectAura1SlotSource::Missing)
        .collect::<Vec<_>>();

    for instruction in &plan.streams {
        let Some(slot) = instruction.target_slot else {
            continue;
        };
        let slot = usize::from(slot);
        if slot >= field_count {
            return Err(AuraError::InvalidValue("target slot"));
        }
        let values = stream_values
            .get(&instruction.stream_id)
            .ok_or(AuraError::InvalidValue("stream body"))?;
        if values.len() != record_count {
            return Err(AuraError::InvalidValue("stream value count"));
        }
        sources[slot] = DirectAura1SlotSource::Direct(values.as_slice());
    }

    for group in &plan.groups {
        match group {
            GenericGroupInstruction::PartitionRunLengths {
                group_id,
                partition_slot,
                ..
            } => {
                let slot = usize::from(*partition_slot);
                if slot >= field_count {
                    return Err(AuraError::InvalidValue("partition slot"));
                }
                let runs = partition_runs
                    .get(group_id)
                    .ok_or(AuraError::InvalidValue("partition run reference"))?;
                sources[slot] = DirectAura1SlotSource::Partition { runs, run_index: 0 };
            }
            GenericGroupInstruction::GroupValueStream {
                parent_group_id,
                output_slot,
                stream_id,
                ..
            } => {
                let slot = usize::from(*output_slot);
                if slot >= field_count {
                    return Err(AuraError::InvalidValue("target slot"));
                }
                let ranges = event_ranges_from_partition_runs(
                    plan,
                    stream_values,
                    partition_runs,
                    *parent_group_id,
                )?;
                let values = stream_values
                    .get(stream_id)
                    .ok_or(AuraError::InvalidValue("group value stream"))?;
                if values.len() != ranges.len() {
                    return Err(AuraError::InvalidValue("group value stream"));
                }
                sources[slot] = DirectAura1SlotSource::GroupValue {
                    ranges,
                    values,
                    range_index: 0,
                };
            }
            GenericGroupInstruction::SegmentedDeltaStream {
                parent_group_id,
                output_slot,
                base_stream_id,
                first_stream_id,
                delta_stream_id,
                ..
            } => {
                let slot = usize::from(*output_slot);
                if slot >= field_count {
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
                sources[slot] = DirectAura1SlotSource::SegmentedDelta {
                    runs,
                    first_values,
                    delta_values,
                    base_by_partition,
                    run_index: 0,
                    delta_index: 0,
                    current_value: 0,
                    initialized: false,
                };
            }
            GenericGroupInstruction::SparseStream {
                presence_group_id,
                output_slot,
                presence_index,
                stream_id,
                ..
            } => {
                let slot = usize::from(*output_slot);
                if slot >= field_count {
                    return Err(AuraError::InvalidValue("target slot"));
                }
                let masks = presence_maps
                    .get(presence_group_id)
                    .ok_or(AuraError::InvalidValue("presence map reference"))?;
                let values = stream_values
                    .get(stream_id)
                    .ok_or(AuraError::InvalidValue("stream body"))?;
                sources[slot] = DirectAura1SlotSource::Sparse {
                    masks,
                    bit: presence_bit_mask(*presence_index)?,
                    values,
                    value_index: 0,
                };
            }
            GenericGroupInstruction::PresenceValue {
                presence_group_id,
                output_slot,
                presence_index,
                value,
                ..
            } => {
                let slot = usize::from(*output_slot);
                if slot >= field_count {
                    return Err(AuraError::InvalidValue("target slot"));
                }
                let masks = presence_maps
                    .get(presence_group_id)
                    .ok_or(AuraError::InvalidValue("presence map reference"))?;
                sources[slot] = DirectAura1SlotSource::PresenceValue {
                    masks,
                    bit: presence_bit_mask(*presence_index)?,
                    value: *value,
                };
            }
            GenericGroupInstruction::Group { .. } | GenericGroupInstruction::PresenceMap { .. } => {
            }
            GenericGroupInstruction::PartitionRuns { .. }
            | GenericGroupInstruction::DerivedStream { .. }
            | GenericGroupInstruction::ExpressionStream { .. }
            | GenericGroupInstruction::ExpressionValue { .. } => return Ok(sources),
        }
    }

    Ok(sources)
}

enum DirectAura1SlotSource<'a> {
    Missing,
    Direct(&'a [i64]),
    Partition {
        runs: &'a [PartitionRun],
        run_index: usize,
    },
    GroupValue {
        ranges: Vec<(usize, usize)>,
        values: &'a [i64],
        range_index: usize,
    },
    SegmentedDelta {
        runs: &'a [PartitionRun],
        first_values: &'a [i64],
        delta_values: &'a [i64],
        base_by_partition: Option<BTreeMap<i64, i64>>,
        run_index: usize,
        delta_index: usize,
        current_value: i64,
        initialized: bool,
    },
    Sparse {
        masks: &'a [i64],
        bit: i64,
        values: &'a [i64],
        value_index: usize,
    },
    PresenceValue {
        masks: &'a [i64],
        bit: i64,
        value: i64,
    },
}

impl<'a> DirectAura1SlotSource<'a> {
    fn is_supported(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    fn value_at(&mut self, row_index: usize) -> Result<i64> {
        match self {
            Self::Missing => Err(AuraError::InvalidValue("target slot")),
            Self::Direct(values) => values
                .get(row_index)
                .copied()
                .ok_or(AuraError::InvalidValue("stream value count")),
            Self::Partition { runs, run_index } => {
                while *run_index < runs.len() && row_index >= runs[*run_index].end {
                    *run_index += 1;
                }
                let run = runs
                    .get(*run_index)
                    .ok_or(AuraError::InvalidValue("partition run"))?;
                if row_index < run.start || row_index >= run.end {
                    return Err(AuraError::InvalidValue("partition run"));
                }
                Ok(run.value)
            }
            Self::GroupValue {
                ranges,
                values,
                range_index,
            } => {
                while *range_index < ranges.len() && row_index >= ranges[*range_index].1 {
                    *range_index += 1;
                }
                let (start, end) = *ranges
                    .get(*range_index)
                    .ok_or(AuraError::InvalidValue("group value stream"))?;
                if row_index < start || row_index >= end {
                    return Err(AuraError::InvalidValue("group value stream"));
                }
                values
                    .get(*range_index)
                    .copied()
                    .ok_or(AuraError::InvalidValue("group value stream"))
            }
            Self::SegmentedDelta {
                runs,
                first_values,
                delta_values,
                base_by_partition,
                run_index,
                delta_index,
                current_value,
                initialized,
            } => {
                while *run_index < runs.len() && row_index >= runs[*run_index].end {
                    *run_index += 1;
                    *initialized = false;
                }
                let run = runs
                    .get(*run_index)
                    .ok_or(AuraError::InvalidValue("partition run"))?;
                if row_index < run.start || row_index >= run.end {
                    return Err(AuraError::InvalidValue("partition run"));
                }
                if row_index == run.start {
                    let first = first_values
                        .get(*run_index)
                        .copied()
                        .ok_or(AuraError::InvalidValue("segmented first stream"))?;
                    *current_value = if let Some(base_by_partition) = base_by_partition {
                        let base = base_by_partition
                            .get(&run.value)
                            .copied()
                            .ok_or(AuraError::InvalidValue("segmented base stream"))?;
                        checked_sum(base, first)?
                    } else {
                        first
                    };
                    *initialized = true;
                    return Ok(*current_value);
                }
                if !*initialized {
                    return Err(AuraError::InvalidValue("segmented delta stream"));
                }
                let delta = delta_values
                    .get(*delta_index)
                    .copied()
                    .ok_or(AuraError::InvalidValue("segmented delta stream"))?;
                *current_value = checked_sum(*current_value, delta)?;
                *delta_index += 1;
                Ok(*current_value)
            }
            Self::Sparse {
                masks,
                bit,
                values,
                value_index,
            } => {
                let mask = *masks
                    .get(row_index)
                    .ok_or(AuraError::InvalidValue("presence stream body"))?;
                if mask < 0 {
                    return Err(AuraError::InvalidValue("presence bit"));
                }
                if mask & *bit == 0 {
                    return Ok(0);
                }
                let value = values
                    .get(*value_index)
                    .copied()
                    .ok_or(AuraError::InvalidValue("sparse stream body"))?;
                *value_index += 1;
                Ok(value)
            }
            Self::PresenceValue { masks, bit, value } => {
                let mask = *masks
                    .get(row_index)
                    .ok_or(AuraError::InvalidValue("presence stream body"))?;
                if mask < 0 {
                    return Err(AuraError::InvalidValue("presence bit"));
                }
                Ok(if mask & *bit != 0 { *value } else { 0 })
            }
        }
    }

    fn finish(&mut self) -> Result<()> {
        match self {
            Self::SegmentedDelta {
                delta_values,
                delta_index,
                ..
            } if *delta_index != delta_values.len() => {
                Err(AuraError::InvalidValue("segmented delta stream"))
            }
            Self::Sparse {
                values,
                value_index,
                ..
            } if *value_index != values.len() => Err(AuraError::InvalidValue("sparse stream body")),
            _ => Ok(()),
        }
    }
}

fn write_direct_i64_width(out: &mut Vec<u8>, value: i64, width: PhysicalWidth) -> Result<()> {
    match width {
        PhysicalWidth::Zero => {
            if value == 0 {
                Ok(())
            } else {
                Err(AuraError::InvalidValue("zero-width value"))
            }
        }
        PhysicalWidth::I8 => {
            let value = i8::try_from(value).map_err(|_| AuraError::InvalidValue("i8 value"))?;
            out.push(value as u8);
            Ok(())
        }
        PhysicalWidth::I16 => {
            let value = i16::try_from(value).map_err(|_| AuraError::InvalidValue("i16 value"))?;
            out.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        PhysicalWidth::I32 => {
            let value = i32::try_from(value).map_err(|_| AuraError::InvalidValue("i32 value"))?;
            out.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        PhysicalWidth::I64 => {
            out.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        PhysicalWidth::I128 => {
            out.extend_from_slice(&i128::from(value).to_le_bytes());
            Ok(())
        }
    }
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
                let bit = presence_bit_mask(*presence_index)?;
                let mut values = values.iter().copied();
                for (row_index, mask) in masks.iter().copied().enumerate() {
                    if mask < 0 {
                        return Err(AuraError::InvalidValue("presence bit"));
                    }
                    if mask & bit != 0 {
                        rows[row_index][output_slot] = values
                            .next()
                            .ok_or(AuraError::InvalidValue("sparse stream body"))?;
                    } else {
                        rows[row_index][output_slot] = 0;
                    }
                    filled[row_index][output_slot] = true;
                }
                if values.next().is_some() {
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
                let bit = presence_bit_mask(*presence_index)?;
                for (row_index, mask) in masks.iter().copied().enumerate() {
                    if mask < 0 {
                        return Err(AuraError::InvalidValue("presence bit"));
                    }
                    rows[row_index][output_slot] = if mask & bit != 0 { *value } else { 0 };
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
            } => Some(PendingDerivedInstruction::Residual {
                output_slot: *output_slot,
                op: *op,
                input_slots: input_slots.as_slice(),
                stream_id: *stream_id,
            }),
            GenericGroupInstruction::ExpressionStream {
                output_slot,
                op,
                input_slots,
                literals,
                stream_id,
                ..
            } => Some(PendingDerivedInstruction::Expression {
                output_slot: *output_slot,
                op: *op,
                input_slots: input_slots.as_slice(),
                literals: literals.as_slice(),
                stream_id: *stream_id,
            }),
            GenericGroupInstruction::ExpressionValue {
                output_slot,
                op,
                input_slots,
                literals,
                residual,
                ..
            } => Some(PendingDerivedInstruction::ExpressionValue {
                output_slot: *output_slot,
                op: *op,
                input_slots: input_slots.as_slice(),
                literals: literals.as_slice(),
                residual: *residual,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    for _ in 0..encoded.field_count.saturating_mul(2).saturating_add(1) {
        let mut progress = false;
        for row_index in 0..encoded.record_count {
            for derived in &derived {
                let (output_slot, stream_id) = match derived {
                    PendingDerivedInstruction::Residual {
                        output_slot,
                        stream_id,
                        ..
                    }
                    | PendingDerivedInstruction::Expression {
                        output_slot,
                        stream_id,
                        ..
                    } => (*output_slot, Some(*stream_id)),
                    PendingDerivedInstruction::ExpressionValue { output_slot, .. } => {
                        (*output_slot, None)
                    }
                };
                let output_slot = usize::from(output_slot);
                if output_slot >= encoded.field_count || filled[row_index][output_slot] {
                    continue;
                }
                if !pending_derived_inputs_ready(derived, row_index, &filled) {
                    continue;
                }
                let residual = if let Some(stream_id) = stream_id {
                    let values = stream_values
                        .get(&stream_id)
                        .ok_or(AuraError::InvalidValue("stream body"))?;
                    if values.len() != encoded.record_count {
                        return Err(AuraError::InvalidValue("stream value count"));
                    }
                    values[row_index]
                } else {
                    match derived {
                        PendingDerivedInstruction::ExpressionValue { residual, .. } => *residual,
                        _ => unreachable!(),
                    }
                };
                rows[row_index][output_slot] =
                    derive_pending_value(derived, row_index, residual, &rows)?;
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

    if let Some((output_slot, op, input_slots, literals)) =
        plan.groups.iter().find_map(|group| match group {
            GenericGroupInstruction::ExpressionStream {
                output_slot,
                op,
                input_slots,
                literals,
                stream_id,
                ..
            } if *stream_id == instruction.stream_id => {
                Some((*output_slot, *op, input_slots, literals))
            }
            _ => None,
        })
    {
        return rows
            .iter()
            .enumerate()
            .map(|(row_index, _)| {
                inverse_expression_value(op, input_slots, literals, output_slot, row_index, rows)
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

fn presence_maps_by_group<'a>(
    plan: &GenericInstructionPlan,
    stream_values: &'a BTreeMap<u16, Vec<i64>>,
    record_count: usize,
) -> Result<BTreeMap<u16, &'a [i64]>> {
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
        out.insert(*group_id, values.as_slice());
    }
    Ok(out)
}

fn presence_bit_mask(index: u16) -> Result<i64> {
    if index >= 62 {
        return Err(AuraError::InvalidValue("presence bit"));
    }
    let bit = 1i64
        .checked_shl(u32::from(index))
        .ok_or(AuraError::InvalidValue("presence bit"))?;
    Ok(bit)
}

fn materialize_generic_i64_columns(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    record_count: usize,
    field_count: usize,
) -> Result<Option<Vec<Vec<i64>>>> {
    let mut columns = (0..field_count).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut filled_slots = vec![false; field_count];

    for instruction in &plan.streams {
        let Some(slot) = instruction.target_slot else {
            continue;
        };
        let slot = usize::from(slot);
        if slot >= field_count {
            return Err(AuraError::InvalidValue("target slot"));
        }
        let values = stream_values
            .get(&instruction.stream_id)
            .ok_or(AuraError::InvalidValue("stream body"))?;
        if values.len() != record_count {
            return Err(AuraError::InvalidValue("stream value count"));
        }
        columns[slot] = values.clone();
        filled_slots[slot] = true;
    }

    let partition_runs = materialize_partition_run_length_columns(
        plan,
        stream_values,
        record_count,
        field_count,
        &mut columns,
        &mut filled_slots,
    )?;
    materialize_group_value_columns(
        plan,
        stream_values,
        &partition_runs,
        record_count,
        field_count,
        &mut columns,
        &mut filled_slots,
    )?;
    materialize_segmented_delta_columns(
        plan,
        stream_values,
        &partition_runs,
        record_count,
        field_count,
        &mut columns,
        &mut filled_slots,
    )?;
    materialize_presence_columns(
        plan,
        stream_values,
        record_count,
        field_count,
        &mut columns,
        &mut filled_slots,
    )?;

    if filled_slots.iter().all(|slot| *slot)
        && columns.iter().all(|column| column.len() == record_count)
    {
        Ok(Some(columns))
    } else {
        Ok(None)
    }
}

fn materialize_partition_run_length_columns(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    record_count: usize,
    field_count: usize,
    columns: &mut [Vec<i64>],
    filled_slots: &mut [bool],
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
        let build_column = columns[partition_slot].is_empty();
        if !build_column && columns[partition_slot].len() != record_count {
            return Err(AuraError::InvalidValue("record count"));
        }
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
            if build_column {
                if columns[partition_slot].len() != row_index {
                    return Err(AuraError::InvalidValue("partition run length"));
                }
                columns[partition_slot].resize(end, value);
            } else {
                columns[partition_slot][row_index..end].fill(value);
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
        filled_slots[partition_slot] = true;
        out.insert(*group_id, runs);
    }
    Ok(out)
}

fn materialize_group_value_columns(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    partition_runs: &BTreeMap<u16, Vec<PartitionRun>>,
    record_count: usize,
    field_count: usize,
    columns: &mut [Vec<i64>],
    filled_slots: &mut [bool],
) -> Result<()> {
    let mut event_ranges_by_group = BTreeMap::new();
    for group in &plan.groups {
        let GenericGroupInstruction::GroupValueStream {
            parent_group_id, ..
        } = group
        else {
            continue;
        };
        if !event_ranges_by_group.contains_key(parent_group_id) {
            event_ranges_by_group.insert(
                *parent_group_id,
                event_ranges_from_partition_runs(
                    plan,
                    stream_values,
                    partition_runs,
                    *parent_group_id,
                )?,
            );
        }
    }

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
        let event_ranges = event_ranges_by_group
            .get(parent_group_id)
            .ok_or(AuraError::InvalidValue("partition run reference"))?;
        let values = stream_values
            .get(stream_id)
            .ok_or(AuraError::InvalidValue("group value stream"))?;
        if values.len() != event_ranges.len() {
            return Err(AuraError::InvalidValue("group value stream"));
        }
        if columns[output_slot].is_empty() {
            for ((start, end), value) in event_ranges.iter().copied().zip(values.iter().copied()) {
                if start > end || end > record_count || start < columns[output_slot].len() {
                    return Err(AuraError::InvalidValue("group value stream"));
                }
                if columns[output_slot].len() < start {
                    columns[output_slot].resize(start, 0);
                }
                columns[output_slot].resize(end, value);
            }
            if columns[output_slot].len() < record_count {
                columns[output_slot].resize(record_count, 0);
            }
        } else {
            if columns[output_slot].len() != record_count {
                return Err(AuraError::InvalidValue("record count"));
            }
            for ((start, end), value) in event_ranges.iter().copied().zip(values.iter().copied()) {
                columns[output_slot][start..end].fill(value);
            }
        }
        filled_slots[output_slot] = true;
    }
    Ok(())
}

fn materialize_segmented_delta_columns(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    partition_runs: &BTreeMap<u16, Vec<PartitionRun>>,
    record_count: usize,
    field_count: usize,
    columns: &mut [Vec<i64>],
    filled_slots: &mut [bool],
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
        let build_column = columns[output_slot].is_empty();
        if !build_column && columns[output_slot].len() != record_count {
            return Err(AuraError::InvalidValue("record count"));
        }
        for (run, first_value) in runs.iter().zip(first_values.iter().copied()) {
            if run.start >= run.end || run.end > record_count {
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
            if build_column {
                if columns[output_slot].len() < run.start {
                    columns[output_slot].resize(run.start, 0);
                } else if columns[output_slot].len() > run.start {
                    return Err(AuraError::InvalidValue("partition run"));
                }
                columns[output_slot].push(value);
            } else {
                columns[output_slot][run.start] = value;
            }
            for row_index in run.start + 1..run.end {
                let delta = *delta_values
                    .get(delta_index)
                    .ok_or(AuraError::InvalidValue("segmented delta stream"))?;
                value = checked_sum(value, delta)?;
                if build_column {
                    columns[output_slot].push(value);
                } else {
                    columns[output_slot][row_index] = value;
                }
                delta_index += 1;
            }
        }
        if delta_index != delta_values.len() {
            return Err(AuraError::InvalidValue("segmented delta stream"));
        }
        if columns[output_slot].len() < record_count {
            columns[output_slot].resize(record_count, 0);
        }
        filled_slots[output_slot] = true;
    }
    Ok(())
}

fn materialize_presence_columns(
    plan: &GenericInstructionPlan,
    stream_values: &BTreeMap<u16, Vec<i64>>,
    record_count: usize,
    field_count: usize,
    columns: &mut [Vec<i64>],
    filled_slots: &mut [bool],
) -> Result<()> {
    let presence_maps = presence_maps_by_group(plan, stream_values, record_count)?;
    for group in &plan.groups {
        match group {
            GenericGroupInstruction::SparseStream {
                presence_group_id,
                output_slot,
                presence_index,
                stream_id,
                ..
            } => {
                let output_slot = usize::from(*output_slot);
                if output_slot >= field_count {
                    return Err(AuraError::InvalidValue("target slot"));
                }
                let masks = presence_maps
                    .get(presence_group_id)
                    .ok_or(AuraError::InvalidValue("presence map reference"))?;
                let values = stream_values
                    .get(stream_id)
                    .ok_or(AuraError::InvalidValue("stream body"))?;
                let bit = presence_bit_mask(*presence_index)?;
                if columns[output_slot].is_empty() {
                    columns[output_slot].resize(record_count, 0);
                } else if columns[output_slot].len() != record_count {
                    return Err(AuraError::InvalidValue("record count"));
                }
                let mut values = values.iter().copied();
                for (row_index, mask) in masks.iter().copied().enumerate() {
                    if mask < 0 {
                        return Err(AuraError::InvalidValue("presence bit"));
                    }
                    if mask & bit != 0 {
                        columns[output_slot][row_index] = values
                            .next()
                            .ok_or(AuraError::InvalidValue("sparse stream body"))?;
                    }
                }
                if values.next().is_some() {
                    return Err(AuraError::InvalidValue("sparse stream body"));
                }
                filled_slots[output_slot] = true;
            }
            GenericGroupInstruction::PresenceValue {
                presence_group_id,
                output_slot,
                presence_index,
                value,
                ..
            } => {
                let output_slot = usize::from(*output_slot);
                if output_slot >= field_count {
                    return Err(AuraError::InvalidValue("target slot"));
                }
                let masks = presence_maps
                    .get(presence_group_id)
                    .ok_or(AuraError::InvalidValue("presence map reference"))?;
                let bit = presence_bit_mask(*presence_index)?;
                if columns[output_slot].is_empty() {
                    columns[output_slot].resize(record_count, 0);
                } else if columns[output_slot].len() != record_count {
                    return Err(AuraError::InvalidValue("record count"));
                }
                for (row_index, mask) in masks.iter().copied().enumerate() {
                    if mask < 0 {
                        return Err(AuraError::InvalidValue("presence bit"));
                    }
                    if mask & bit != 0 {
                        columns[output_slot][row_index] = *value;
                    }
                }
                filled_slots[output_slot] = true;
            }
            _ => {}
        }
    }
    Ok(())
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
    add_schema_derived_hints(schema, rows, &mut state)?;
    add_group_value_hints(schema, rows, &mut state, partition_runs.as_ref())?;
    add_segmented_delta_hints(schema, rows, &mut state, partition_runs.as_ref())?;
    add_sparse_presence_hints(schema, rows, &mut state)?;

    for field in &schema.fields {
        if state.planned_slots.contains(&field.index) {
            continue;
        }
        let values = column_values(rows, field.index)?;
        match field.relation {
            FieldRelation::DeltaFromField(parent_slot) => {
                let candidate = best_parent_candidate(field.index, parent_slot, values, rows)?;
                state.add_slot_candidate(field.index, candidate)?;
            }
            FieldRelation::None => {
                state.add_stream(Some(field.index), values)?;
                state.planned_slots.insert(field.index);
            }
        }
    }

    state.finish()
}

fn add_schema_derived_hints(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    state: &mut PlannerState,
) -> Result<()> {
    schema.validate_derived_expressions()?;
    for expression in &schema.derived_expressions {
        if state.planned_slots.contains(&expression.output_slot) {
            return Err(AuraError::InvalidValue("derived expression output"));
        }
        match derived_expression_op(expression)? {
            DeclaredExpressionPlanOp::Residual(op) => {
                let candidate = best_declared_residual_candidate(expression, op, rows)?;
                state.add_slot_candidate(expression.output_slot, candidate)?;
            }
            DeclaredExpressionPlanOp::Expression(op) => {
                let candidate = best_declared_expression_candidate(expression, op, rows)?;
                state.add_slot_candidate(expression.output_slot, candidate)?;
            }
        }
    }
    Ok(())
}

fn constant_residual(values: &[i64]) -> Option<i64> {
    let first = values.first().copied().unwrap_or(0);
    values.iter().all(|value| *value == first).then_some(first)
}

fn best_parent_candidate(
    output_slot: u16,
    parent_slot: u16,
    values: Vec<i64>,
    rows: &[Vec<i64>],
) -> Result<SlotPlanCandidate> {
    let mut candidates = vec![direct_candidate(values.clone())?];
    let parent_values = column_values(rows, parent_slot)?;
    let same_row_residuals = values
        .iter()
        .zip(&parent_values)
        .map(|(value, parent)| checked_delta(*value, *parent))
        .collect::<Result<Vec<_>>>()?;
    candidates.push(derived_candidate(
        output_slot,
        DerivedOp::AddResidual,
        vec![parent_slot],
        same_row_residuals,
    )?);

    let subtract_residuals = values
        .iter()
        .zip(&parent_values)
        .map(|(value, parent)| checked_delta(*parent, *value))
        .collect::<Result<Vec<_>>>()?;
    candidates.push(derived_candidate(
        output_slot,
        DerivedOp::SubtractResidual,
        vec![parent_slot],
        subtract_residuals,
    )?);

    if !values.is_empty() {
        let mut previous_parent_residuals = Vec::with_capacity(values.len());
        previous_parent_residuals.push(values[0]);
        for (value, previous_parent) in values.iter().skip(1).zip(parent_values.iter()) {
            previous_parent_residuals.push(checked_delta(*value, *previous_parent)?);
        }
        candidates.push(derived_candidate(
            output_slot,
            DerivedOp::FirstOffsetThenDelta,
            vec![parent_slot],
            previous_parent_residuals,
        )?);
    }

    best_slot_candidate(candidates)
}

fn best_declared_residual_candidate(
    expression: &DerivedExpression,
    op: DerivedOp,
    rows: &[Vec<i64>],
) -> Result<SlotPlanCandidate> {
    let values = column_values(rows, expression.output_slot)?;
    let residuals = derived_expression_residuals(expression, rows)?;
    best_slot_candidate(vec![
        direct_candidate(values)?,
        derived_candidate(
            expression.output_slot,
            op,
            expression.input_slots.clone(),
            residuals,
        )?,
    ])
}

fn best_declared_expression_candidate(
    expression: &DerivedExpression,
    op: DerivedExpressionOp,
    rows: &[Vec<i64>],
) -> Result<SlotPlanCandidate> {
    let values = column_values(rows, expression.output_slot)?;
    let residuals = expression_stream_residuals(expression, rows)?;
    let expression_candidate = if let Some(residual) = constant_residual(&residuals) {
        expression_value_candidate(
            expression.output_slot,
            op,
            expression.input_slots.clone(),
            expression.literals.clone(),
            residual,
        )?
    } else {
        expression_candidate(
            expression.output_slot,
            op,
            expression.input_slots.clone(),
            expression.literals.clone(),
            residuals,
        )?
    };
    best_slot_candidate(vec![direct_candidate(values)?, expression_candidate])
}

fn direct_candidate(values: Vec<i64>) -> Result<SlotPlanCandidate> {
    Ok(SlotPlanCandidate::Direct {
        score: encoded_i64_score(&values)?,
        values,
    })
}

fn derived_candidate(
    output_slot: u16,
    op: DerivedOp,
    input_slots: Vec<u16>,
    values: Vec<i64>,
) -> Result<SlotPlanCandidate> {
    let group_score = derived_group_score(output_slot, op, &input_slots)?;
    Ok(SlotPlanCandidate::Derived {
        score: encoded_i64_score(&values)?.saturating_add(group_score),
        op,
        input_slots,
        values,
    })
}

fn expression_candidate(
    output_slot: u16,
    op: DerivedExpressionOp,
    input_slots: Vec<u16>,
    literals: Vec<i64>,
    values: Vec<i64>,
) -> Result<SlotPlanCandidate> {
    let group_score = expression_group_score(output_slot, op, &input_slots, &literals)?;
    Ok(SlotPlanCandidate::Expression {
        score: encoded_i64_score(&values)?.saturating_add(group_score),
        op,
        input_slots,
        literals,
        values,
    })
}

fn expression_value_candidate(
    output_slot: u16,
    op: DerivedExpressionOp,
    input_slots: Vec<u16>,
    literals: Vec<i64>,
    residual: i64,
) -> Result<SlotPlanCandidate> {
    let score = expression_value_group_score(output_slot, op, &input_slots, &literals, residual)?;
    Ok(SlotPlanCandidate::ExpressionValue {
        score,
        op,
        input_slots,
        literals,
        residual,
    })
}

fn best_slot_candidate(candidates: Vec<SlotPlanCandidate>) -> Result<SlotPlanCandidate> {
    candidates
        .into_iter()
        .min_by_key(|candidate| (candidate.score(), slot_candidate_preference(candidate)))
        .ok_or(AuraError::InvalidValue("slot candidate"))
}

fn slot_candidate_preference(candidate: &SlotPlanCandidate) -> u8 {
    match candidate {
        SlotPlanCandidate::Direct { .. } => 0,
        SlotPlanCandidate::ExpressionValue { .. } => 1,
        SlotPlanCandidate::Derived { .. } => 2,
        SlotPlanCandidate::Expression { .. } => 3,
    }
}

fn encoded_i64_score(values: &[i64]) -> Result<usize> {
    let op = choose_i64_op(values)?;
    encoded_i64_score_with_op(&op, values)
}

fn derived_group_score(output_slot: u16, op: DerivedOp, input_slots: &[u16]) -> Result<usize> {
    group_score_with_optional_stream(GenericGroupInstruction::DerivedStream {
        group_id: 0,
        parent_group_id: None,
        output_slot,
        op,
        input_slots: input_slots.to_vec(),
        stream_id: 0,
    })
}

fn expression_group_score(
    output_slot: u16,
    op: DerivedExpressionOp,
    input_slots: &[u16],
    literals: &[i64],
) -> Result<usize> {
    group_score_with_optional_stream(GenericGroupInstruction::ExpressionStream {
        group_id: 0,
        parent_group_id: None,
        output_slot,
        op,
        input_slots: input_slots.to_vec(),
        literals: literals.to_vec(),
        stream_id: 0,
    })
}

fn expression_value_group_score(
    output_slot: u16,
    op: DerivedExpressionOp,
    input_slots: &[u16],
    literals: &[i64],
    residual: i64,
) -> Result<usize> {
    group_score(GenericGroupInstruction::ExpressionValue {
        group_id: 0,
        parent_group_id: None,
        output_slot,
        op,
        input_slots: input_slots.to_vec(),
        literals: literals.to_vec(),
        residual,
    })
}

fn group_score_with_optional_stream(group: GenericGroupInstruction) -> Result<usize> {
    let stream = GenericStreamInstruction {
        stream_id: 0,
        target_slot: None,
        op: GenericStreamOp::FixedStep { base: 0, step: 0 },
    };
    let without_group = GenericInstructionPlan {
        streams: vec![stream.clone()],
        groups: Vec::new(),
    }
    .encode()?
    .len();
    let with_group = GenericInstructionPlan {
        streams: vec![stream],
        groups: vec![group],
    }
    .encode()?
    .len();
    Ok(with_group.saturating_sub(without_group))
}

fn group_score(group: GenericGroupInstruction) -> Result<usize> {
    let without_group = GenericInstructionPlan {
        streams: Vec::new(),
        groups: Vec::new(),
    }
    .encode()?
    .len();
    let with_group = GenericInstructionPlan {
        streams: Vec::new(),
        groups: vec![group],
    }
    .encode()?
    .len();
    Ok(with_group.saturating_sub(without_group))
}

enum DeclaredExpressionPlanOp {
    Residual(DerivedOp),
    Expression(DerivedExpressionOp),
}

fn derived_expression_op(expression: &DerivedExpression) -> Result<DeclaredExpressionPlanOp> {
    match expression.op {
        DerivedExpressionOp::AddResidual => {
            Ok(DeclaredExpressionPlanOp::Residual(DerivedOp::AddResidual))
        }
        DerivedExpressionOp::SubtractResidual => Ok(DeclaredExpressionPlanOp::Residual(
            DerivedOp::SubtractResidual,
        )),
        DerivedExpressionOp::MaxPlusResidual => Ok(DeclaredExpressionPlanOp::Residual(
            DerivedOp::MaxPlusResidual,
        )),
        DerivedExpressionOp::MinMinusResidual => Ok(DeclaredExpressionPlanOp::Residual(
            DerivedOp::MinMinusResidual,
        )),
        DerivedExpressionOp::FirstOffsetThenDelta => Ok(DeclaredExpressionPlanOp::Residual(
            DerivedOp::FirstOffsetThenDelta,
        )),
        DerivedExpressionOp::Add
        | DerivedExpressionOp::Sub
        | DerivedExpressionOp::Mul
        | DerivedExpressionOp::Div
        | DerivedExpressionOp::Min
        | DerivedExpressionOp::Max => Ok(DeclaredExpressionPlanOp::Expression(expression.op)),
    }
}

fn derived_expression_residuals(
    expression: &DerivedExpression,
    rows: &[Vec<i64>],
) -> Result<Vec<i64>> {
    rows.iter()
        .enumerate()
        .map(|(row_index, _)| inverse_declared_derive_value(expression, row_index, rows))
        .collect()
}

fn expression_stream_residuals(
    expression: &DerivedExpression,
    rows: &[Vec<i64>],
) -> Result<Vec<i64>> {
    rows.iter()
        .enumerate()
        .map(|(row_index, _)| {
            let output = rows[row_index][usize::from(expression.output_slot)];
            let predicted = evaluate_expression_terms(
                expression.op,
                &expression.input_slots,
                &expression.literals,
                row_index,
                rows,
            )?;
            checked_delta(output, predicted)
        })
        .collect()
}

fn inverse_declared_derive_value(
    expression: &DerivedExpression,
    row_index: usize,
    rows: &[Vec<i64>],
) -> Result<i64> {
    let output = rows
        .get(row_index)
        .and_then(|row| row.get(usize::from(expression.output_slot)))
        .copied()
        .ok_or(AuraError::InvalidValue("output slot"))?;
    match expression.op {
        DerivedExpressionOp::AddResidual => {
            let base = rows[row_index][usize::from(expression.input_slots[0])];
            checked_delta(output, base)
        }
        DerivedExpressionOp::SubtractResidual => {
            let base = rows[row_index][usize::from(expression.input_slots[0])];
            checked_delta(base, output)
        }
        DerivedExpressionOp::MaxPlusResidual => {
            let base = expression
                .input_slots
                .iter()
                .map(|slot| rows[row_index][usize::from(*slot)])
                .max()
                .ok_or(AuraError::InvalidValue("input slots"))?;
            checked_delta(output, base)
        }
        DerivedExpressionOp::MinMinusResidual => {
            let base = expression
                .input_slots
                .iter()
                .map(|slot| rows[row_index][usize::from(*slot)])
                .min()
                .ok_or(AuraError::InvalidValue("input slots"))?;
            checked_delta(base, output)
        }
        DerivedExpressionOp::FirstOffsetThenDelta => {
            if row_index == 0 {
                Ok(output)
            } else {
                let base = rows[row_index - 1][usize::from(expression.input_slots[0])];
                checked_delta(output, base)
            }
        }
        DerivedExpressionOp::Add
        | DerivedExpressionOp::Sub
        | DerivedExpressionOp::Mul
        | DerivedExpressionOp::Div
        | DerivedExpressionOp::Min
        | DerivedExpressionOp::Max => Err(AuraError::InvalidValue("derived expression op")),
    }
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
        if sparse_candidate_better(&candidate, best.as_ref()) {
            best = Some(candidate);
        }
    }
    let mut selected = Vec::new();
    for candidate in candidates {
        selected.push(candidate);
        let candidate = sparse_set_candidate(rows, selected.clone())?;
        if sparse_candidate_better(&candidate, best.as_ref()) {
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

fn sparse_candidate_better(
    candidate: &SparseSetCandidate,
    current: Option<&SparseSetCandidate>,
) -> bool {
    if candidate.sparse_size >= candidate.direct_size {
        return false;
    }
    let saved = candidate.direct_size - candidate.sparse_size;
    current.is_none_or(|current| {
        let current_saved = current.direct_size.saturating_sub(current.sparse_size);
        saved > current_saved
            || (saved == current_saved && candidate.sparse_size < current.sparse_size)
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
    if let Some(op) = derive_prev_varint(values)? {
        candidates.push(op);
    }
    candidates.push(derive_patched_bitpack(values)?);
    candidates.push(derive_rle(values)?);
    candidates.push(derive_bitplane_rle(values)?);
    if let Some(op) = derive_dictionary(values)? {
        candidates.push(op);
    }
    if let Some(op) = derive_packed_dictionary(values)? {
        candidates.push(op);
    }
    if let Some(op) = derive_huffman_dictionary(values)? {
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

    let mut scored = candidates
        .into_iter()
        .map(|op| {
            let size = encoded_i64_score_with_op(&op, values)?;
            Ok((size, op_preference(&op), op))
        })
        .collect::<Result<Vec<_>>>()?;
    let best_non_huffman_score = scored
        .iter()
        .filter(|(_, _, op)| !matches!(op, GenericStreamOp::HuffmanDictionary { .. }))
        .map(|(size, _, _)| *size)
        .min()
        .ok_or(AuraError::InvalidValue("stream op"))?;
    scored.retain(|(size, _, op)| {
        !matches!(op, GenericStreamOp::HuffmanDictionary { .. })
            || huffman_clears_speed_gate(*size, best_non_huffman_score)
    });
    scored
        .into_iter()
        .min_by_key(|(size, preference, _)| (*size, *preference))
        .map(|(_, _, op)| op)
        .ok_or(AuraError::InvalidValue("stream op"))
}

fn huffman_clears_speed_gate(huffman_score: usize, best_non_huffman_score: usize) -> bool {
    huffman_score.saturating_mul(2) <= best_non_huffman_score
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

fn encoded_i64_score_with_op(op: &GenericStreamOp, values: &[i64]) -> Result<usize> {
    Ok(encoded_i64_len_with_op(op, values)? + op.encoded_len()?)
}

fn op_preference(op: &GenericStreamOp) -> u8 {
    match op {
        GenericStreamOp::FixedStep { .. } => 0,
        GenericStreamOp::BaseBitpack { .. } => 1,
        GenericStreamOp::PrevDelta { .. } => 2,
        GenericStreamOp::PrevVarint { .. } => 3,
        GenericStreamOp::PatchedBitpack { .. } => 4,
        GenericStreamOp::Rle { .. } => 5,
        GenericStreamOp::BitplaneRle { .. } => 6,
        GenericStreamOp::PackedDictionary { .. } => 7,
        GenericStreamOp::HuffmanDictionary { .. } => 8,
        GenericStreamOp::Dictionary { .. } => 9,
        GenericStreamOp::BlockLocal { .. } => 10,
        GenericStreamOp::UuidConstMask { .. } => 11,
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

fn derive_prev_varint(values: &[i64]) -> Result<Option<GenericStreamOp>> {
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
    Ok(Some(GenericStreamOp::PrevVarint {
        base,
        unit: signed_gcd_unit(&deltas),
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

fn derive_packed_dictionary(values: &[i64]) -> Result<Option<GenericStreamOp>> {
    if values.is_empty() {
        return Ok(None);
    }
    let base = values.iter().copied().min().unwrap_or(0);
    let residuals = unsigned_offsets(values, base)?;
    let unit = storage_unit(&residuals);
    let scaled_values = residuals
        .iter()
        .map(|value| value / unit as u64)
        .collect::<Vec<_>>();
    let mut entries = scaled_values;
    entries.sort_unstable();
    entries.dedup();
    if entries.len() == values.len() {
        return Ok(None);
    }
    let max_entry = entries.iter().copied().max().unwrap_or(0);
    let max_code = entries.len().saturating_sub(1) as u64;
    Ok(Some(GenericStreamOp::PackedDictionary {
        base,
        unit,
        entry_count: u32::try_from(entries.len())
            .map_err(|_| AuraError::InvalidValue("dictionary entry count"))?,
        entry_width: unsigned_bitpack_width(max_entry),
        code_width: unsigned_bitpack_width(max_code),
    }))
}

fn derive_huffman_dictionary(values: &[i64]) -> Result<Option<GenericStreamOp>> {
    if values.is_empty() {
        return Ok(None);
    }
    let base = values.iter().copied().min().unwrap_or(0);
    let residuals = unsigned_offsets(values, base)?;
    let unit = storage_unit(&residuals);
    let scaled_values = residuals
        .iter()
        .map(|value| value / unit as u64)
        .collect::<Vec<_>>();
    let mut entries = scaled_values.clone();
    entries.sort_unstable();
    entries.dedup();
    if entries.len() <= 1 || entries.len() == values.len() {
        return Ok(None);
    }

    let entry_indexes = entries
        .iter()
        .enumerate()
        .map(|(index, value)| (*value, index))
        .collect::<BTreeMap<_, _>>();
    let mut frequencies = vec![0u64; entries.len()];
    for value in &scaled_values {
        let index = entry_indexes
            .get(value)
            .copied()
            .ok_or(AuraError::InvalidValue("dictionary code"))?;
        frequencies[index] += 1;
    }
    let code_lengths = huffman_code_lengths(&frequencies)?;
    let max_entry = entries.iter().copied().max().unwrap_or(0);
    Ok(Some(GenericStreamOp::HuffmanDictionary {
        base,
        unit,
        entry_count: u32::try_from(entries.len())
            .map_err(|_| AuraError::InvalidValue("dictionary entry count"))?,
        entry_width: unsigned_bitpack_width(max_entry),
        code_lengths,
    }))
}

#[derive(Debug)]
struct HuffmanTreeNode {
    symbol: Option<usize>,
    left: Option<usize>,
    right: Option<usize>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct HuffmanHeapNode {
    frequency: u64,
    min_symbol: usize,
    node_index: usize,
}

impl Ord for HuffmanHeapNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .frequency
            .cmp(&self.frequency)
            .then_with(|| other.min_symbol.cmp(&self.min_symbol))
            .then_with(|| other.node_index.cmp(&self.node_index))
    }
}

impl PartialOrd for HuffmanHeapNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn huffman_code_lengths(frequencies: &[u64]) -> Result<Vec<u8>> {
    if frequencies.is_empty() || frequencies.iter().any(|frequency| *frequency == 0) {
        return Err(AuraError::InvalidValue("huffman frequencies"));
    }
    if frequencies.len() == 1 {
        return Ok(vec![0]);
    }

    let mut nodes = Vec::with_capacity(frequencies.len().saturating_mul(2));
    let mut heap = BinaryHeap::new();
    for (symbol, frequency) in frequencies.iter().copied().enumerate() {
        let node_index = nodes.len();
        nodes.push(HuffmanTreeNode {
            symbol: Some(symbol),
            left: None,
            right: None,
        });
        heap.push(HuffmanHeapNode {
            frequency,
            min_symbol: symbol,
            node_index,
        });
    }

    while heap.len() > 1 {
        let left = heap
            .pop()
            .ok_or(AuraError::InvalidValue("huffman frequencies"))?;
        let right = heap
            .pop()
            .ok_or(AuraError::InvalidValue("huffman frequencies"))?;
        let node_index = nodes.len();
        nodes.push(HuffmanTreeNode {
            symbol: None,
            left: Some(left.node_index),
            right: Some(right.node_index),
        });
        heap.push(HuffmanHeapNode {
            frequency: left.frequency.saturating_add(right.frequency),
            min_symbol: left.min_symbol.min(right.min_symbol),
            node_index,
        });
    }

    let root = heap
        .pop()
        .ok_or(AuraError::InvalidValue("huffman frequencies"))?;
    let mut lengths = vec![0u8; frequencies.len()];
    let mut stack = vec![(root.node_index, 0u8)];
    while let Some((node_index, depth)) = stack.pop() {
        let node = nodes
            .get(node_index)
            .ok_or(AuraError::InvalidValue("huffman tree"))?;
        if let Some(symbol) = node.symbol {
            lengths[symbol] = depth;
            continue;
        }
        let next_depth = depth
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("huffman code lengths"))?;
        if next_depth > 64 {
            return Err(AuraError::InvalidValue("huffman code lengths"));
        }
        if let Some(right) = node.right {
            stack.push((right, next_depth));
        }
        if let Some(left) = node.left {
            stack.push((left, next_depth));
        }
    }
    Ok(lengths)
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

fn pending_derived_inputs_ready(
    instruction: &PendingDerivedInstruction<'_>,
    row_index: usize,
    filled: &[Vec<bool>],
) -> bool {
    match instruction {
        PendingDerivedInstruction::Residual {
            op, input_slots, ..
        } => derived_inputs_ready(*op, input_slots, row_index, filled),
        PendingDerivedInstruction::Expression { input_slots, .. } => input_slots
            .iter()
            .all(|slot| filled[row_index][usize::from(*slot)]),
        PendingDerivedInstruction::ExpressionValue { input_slots, .. } => input_slots
            .iter()
            .all(|slot| filled[row_index][usize::from(*slot)]),
    }
}

fn derive_pending_value(
    instruction: &PendingDerivedInstruction<'_>,
    row_index: usize,
    residual: i64,
    rows: &[Vec<i64>],
) -> Result<i64> {
    match instruction {
        PendingDerivedInstruction::Residual {
            op, input_slots, ..
        } => derive_value(*op, input_slots, row_index, residual, rows),
        PendingDerivedInstruction::Expression {
            op,
            input_slots,
            literals,
            ..
        } => {
            let predicted = evaluate_expression_terms(*op, input_slots, literals, row_index, rows)?;
            checked_sum(predicted, residual)
        }
        PendingDerivedInstruction::ExpressionValue {
            op,
            input_slots,
            literals,
            ..
        } => {
            let predicted = evaluate_expression_terms(*op, input_slots, literals, row_index, rows)?;
            checked_sum(predicted, residual)
        }
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

fn inverse_expression_value(
    op: DerivedExpressionOp,
    input_slots: &[u16],
    literals: &[i64],
    output_slot: u16,
    row_index: usize,
    rows: &[Vec<i64>],
) -> Result<i64> {
    let output = rows
        .get(row_index)
        .and_then(|row| row.get(usize::from(output_slot)))
        .copied()
        .ok_or(AuraError::InvalidValue("output slot"))?;
    let predicted = evaluate_expression_terms(op, input_slots, literals, row_index, rows)?;
    checked_delta(output, predicted)
}

fn evaluate_expression_terms(
    op: DerivedExpressionOp,
    input_slots: &[u16],
    literals: &[i64],
    row_index: usize,
    rows: &[Vec<i64>],
) -> Result<i64> {
    let mut terms = input_slots
        .iter()
        .map(|slot| {
            rows.get(row_index)
                .and_then(|row| row.get(usize::from(*slot)))
                .copied()
                .ok_or(AuraError::InvalidValue("input slots"))
        })
        .collect::<Result<Vec<_>>>()?;
    terms.extend_from_slice(literals);
    match op {
        DerivedExpressionOp::Add => checked_add_terms(&terms),
        DerivedExpressionOp::Sub => checked_sub_terms(&terms),
        DerivedExpressionOp::Mul => checked_mul_terms(&terms),
        DerivedExpressionOp::Div => checked_div_terms(&terms),
        DerivedExpressionOp::Min => terms
            .into_iter()
            .min()
            .ok_or(AuraError::InvalidValue("expression terms")),
        DerivedExpressionOp::Max => terms
            .into_iter()
            .max()
            .ok_or(AuraError::InvalidValue("expression terms")),
        DerivedExpressionOp::AddResidual
        | DerivedExpressionOp::SubtractResidual
        | DerivedExpressionOp::MaxPlusResidual
        | DerivedExpressionOp::MinMinusResidual
        | DerivedExpressionOp::FirstOffsetThenDelta => {
            Err(AuraError::InvalidValue("derived expression op"))
        }
    }
}

fn checked_add_terms(terms: &[i64]) -> Result<i64> {
    let sum = terms.iter().try_fold(0i128, |sum, term| {
        sum.checked_add(i128::from(*term))
            .ok_or(AuraError::InvalidValue("expression value"))
    })?;
    i64::try_from(sum).map_err(|_| AuraError::InvalidValue("expression value"))
}

fn checked_sub_terms(terms: &[i64]) -> Result<i64> {
    let Some((first, rest)) = terms.split_first() else {
        return Err(AuraError::InvalidValue("expression terms"));
    };
    let value = rest.iter().try_fold(i128::from(*first), |value, term| {
        value
            .checked_sub(i128::from(*term))
            .ok_or(AuraError::InvalidValue("expression value"))
    })?;
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("expression value"))
}

fn checked_mul_terms(terms: &[i64]) -> Result<i64> {
    let value = terms.iter().try_fold(1i128, |value, term| {
        value
            .checked_mul(i128::from(*term))
            .ok_or(AuraError::InvalidValue("expression value"))
    })?;
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("expression value"))
}

fn checked_div_terms(terms: &[i64]) -> Result<i64> {
    let Some((first, rest)) = terms.split_first() else {
        return Err(AuraError::InvalidValue("expression terms"));
    };
    let value = rest.iter().try_fold(i128::from(*first), |value, term| {
        let divisor = i128::from(*term);
        if divisor == 0 {
            return Err(AuraError::InvalidValue("expression value"));
        }
        value
            .checked_div(divisor)
            .ok_or(AuraError::InvalidValue("expression value"))
    })?;
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("expression value"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_score_counts_compact_huffman_footer_bytes() {
        let values = [10, 10, 10, 20, 10, 30, 40, 20, 10];
        let packed = GenericStreamOp::PackedDictionary {
            base: 10,
            unit: 10,
            entry_count: 4,
            entry_width: 2,
            code_width: 2,
        };
        let huffman = GenericStreamOp::HuffmanDictionary {
            base: 10,
            unit: 10,
            entry_count: 4,
            entry_width: 2,
            code_lengths: vec![1, 2, 3, 3],
        };

        assert!(
            encoded_i64_len_with_op(&huffman, &values).unwrap()
                < encoded_i64_len_with_op(&packed, &values).unwrap()
        );
        assert!(huffman.encoded_len().unwrap() < 1 + 8 + 8 + 4 + 1 + 4);
        let legacy_huffman_score =
            encoded_i64_len_with_op(&huffman, &values).unwrap() + 1 + 8 + 8 + 4 + 1 + 4;
        assert!(encoded_i64_score_with_op(&huffman, &values).unwrap() < legacy_huffman_score);
    }

    #[test]
    fn column_decoder_matches_rows_for_grouped_sparse_plan() {
        let schema = crate::schema::generic_i64_parent_schema(
            "grouped_sparse_decode_v1",
            &[100, 0, 0, 205, 4, 5, 5, 5],
        )
        .unwrap();
        let rows = vec![
            vec![1_000, 10, 20, 0, 100_000, 5, 0, 0],
            vec![1_000, 10, 20, 0, 100_010, 0, 7, 1],
            vec![1_000, 10, 20, 1, 100_020, 9, 0, 0],
            vec![2_000, 11, 21, 0, 100_030, 0, 8, 1],
            vec![2_000, 11, 21, 1, 100_040, 11, 0, 0],
            vec![2_000, 11, 21, 1, 100_050, 0, 0, 1],
        ];
        let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
        let body = encode_generic_i64_rows_body(&encoded).unwrap();

        let columns = try_decode_generic_i64_columns_body(
            encoded.plan.clone(),
            &body,
            rows.len(),
            schema.fields.len(),
        )
        .unwrap()
        .expect("column fast path");
        let column_rows = (0..rows.len())
            .map(|row_index| {
                (0..schema.fields.len())
                    .map(|field_index| columns[field_index][row_index])
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let decoded_rows =
            decode_generic_i64_rows_body(encoded.plan, &body, rows.len(), schema.fields.len())
                .unwrap();

        assert_eq!(rows, decoded_rows);
        assert_eq!(decoded_rows, column_rows);
    }

    #[test]
    fn streaming_aura1_body_matches_direct_body_for_grouped_sparse_plan() {
        let plan = GenericInstructionPlan {
            streams: vec![
                GenericStreamInstruction {
                    stream_id: 0,
                    target_slot: None,
                    op: GenericStreamOp::Dictionary {
                        unit: 1,
                        entry_count: 2,
                        code_width: 1,
                    },
                },
                GenericStreamInstruction {
                    stream_id: 1,
                    target_slot: None,
                    op: GenericStreamOp::Dictionary {
                        unit: 1,
                        entry_count: 1,
                        code_width: 0,
                    },
                },
                GenericStreamInstruction {
                    stream_id: 2,
                    target_slot: None,
                    op: GenericStreamOp::Dictionary {
                        unit: 1,
                        entry_count: 1,
                        code_width: 0,
                    },
                },
                GenericStreamInstruction {
                    stream_id: 3,
                    target_slot: None,
                    op: GenericStreamOp::FixedStep { base: 10, step: 10 },
                },
            ],
            groups: vec![
                GenericGroupInstruction::Group {
                    group_id: 0,
                    event_slots: vec![0],
                    repeated_slots: vec![1],
                },
                GenericGroupInstruction::PartitionRunLengths {
                    group_id: 1,
                    parent_group_id: 0,
                    partition_slot: 1,
                    fixed_order: false,
                    value_stream_id: 0,
                    count_stream_id: 1,
                    event_count_stream_id: Some(2),
                },
                GenericGroupInstruction::GroupValueStream {
                    group_id: 2,
                    parent_group_id: 1,
                    output_slot: 0,
                    stream_id: 3,
                },
            ],
        };
        let streams = vec![
            GenericEncodedStream {
                stream_id: 0,
                value_count: 2,
                body: encode_generic_stream_body(
                    &plan.streams[0],
                    &GenericStreamBodyValue::I64(vec![0, 1]),
                )
                .unwrap(),
            },
            GenericEncodedStream {
                stream_id: 1,
                value_count: 2,
                body: encode_generic_stream_body(
                    &plan.streams[1],
                    &GenericStreamBodyValue::I64(vec![2, 2]),
                )
                .unwrap(),
            },
            GenericEncodedStream {
                stream_id: 2,
                value_count: 2,
                body: encode_generic_stream_body(
                    &plan.streams[2],
                    &GenericStreamBodyValue::I64(vec![1, 1]),
                )
                .unwrap(),
            },
            GenericEncodedStream {
                stream_id: 3,
                value_count: 2,
                body: encode_generic_stream_body(
                    &plan.streams[3],
                    &GenericStreamBodyValue::I64(vec![10, 20]),
                )
                .unwrap(),
            },
        ];
        let encoded = GenericEncodedI64Rows {
            plan,
            streams,
            record_count: 4,
            field_count: 2,
        };
        let aura1_plan = Aura1Plan {
            block_capacity: 1,
            fields: vec![
                crate::plan::PhysicalFieldPlan {
                    field_index: 0,
                    encoding: crate::plan::FieldEncoding::Absolute,
                    width: PhysicalWidth::I8,
                    bit_width: 0,
                    reference_field_index: None,
                    base_value: 0,
                    step: 0,
                    estimated_bytes: 0,
                },
                crate::plan::PhysicalFieldPlan {
                    field_index: 1,
                    encoding: crate::plan::FieldEncoding::Absolute,
                    width: PhysicalWidth::I8,
                    bit_width: 0,
                    reference_field_index: None,
                    base_value: 0,
                    step: 0,
                    estimated_bytes: 0,
                },
            ],
        };
        let body = encode_generic_i64_rows_body(&encoded).unwrap();

        let direct = try_encode_generic_i64_aura1_body(
            encoded.plan.clone(),
            &body,
            encoded.record_count,
            encoded.field_count,
            &aura1_plan,
        )
        .unwrap()
        .expect("direct Aura1 body");
        let streaming = try_encode_generic_i64_aura1_body_streaming(
            encoded.plan.clone(),
            &body,
            encoded.record_count,
            encoded.field_count,
            &aura1_plan,
        )
        .unwrap()
        .unwrap_or_else(|| panic!("streaming Aura1 body for plan {:#?}", encoded.plan));

        assert_eq!(direct, streaming);
    }
}
