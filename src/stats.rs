use crate::schema::{FieldRelation, SchemaDescriptor};
use crate::types::BookEvent;
use crate::{AuraError, Result};

/// Smallest fixed integer width proven safe by ingest stats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PhysicalWidth {
    Zero,
    I8,
    I16,
    I32,
    I64,
    I128,
}

impl PhysicalWidth {
    pub const fn byte_width(self) -> u8 {
        match self {
            Self::Zero => 0,
            Self::I8 => 1,
            Self::I16 => 2,
            Self::I32 => 4,
            Self::I64 => 8,
            Self::I128 => 16,
        }
    }

    pub const fn code(self) -> u8 {
        match self {
            Self::Zero => 0,
            Self::I8 => 1,
            Self::I16 => 2,
            Self::I32 => 3,
            Self::I64 => 4,
            Self::I128 => 5,
        }
    }

    pub fn from_code(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Zero),
            1 => Ok(Self::I8),
            2 => Ok(Self::I16),
            3 => Ok(Self::I32),
            4 => Ok(Self::I64),
            5 => Ok(Self::I128),
            _ => Err(AuraError::InvalidValue("physical width")),
        }
    }
}

/// Observed range and variance for one logical integer field.
#[derive(Debug, Clone, Eq)]
pub struct FieldStats {
    pub field_index: u16,
    pub observed: u64,
    pub min: i64,
    pub max: i64,
    pub first_delta: Option<i64>,
    pub min_delta: i64,
    pub max_delta: i64,
    pub max_abs_delta: u64,
    pub delta_valid: bool,
    pub min_delta2: i64,
    pub max_delta2: i64,
    pub max_abs_delta2: u64,
    pub delta2_valid: bool,
    pub monotonic_non_decreasing: bool,
    pub first_value: Option<i64>,
    pub fixed_step: Option<i64>,
    pub fixed_step_valid: bool,
    pub rough_step: Option<RoughStepStats>,
    pub absolute_zigzag_varint_bytes: u64,
    pub previous_delta_zigzag_varint_bytes: u64,
    pub delta2_zigzag_varint_bytes: u64,
    previous: Option<i64>,
    previous_delta: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoughStepStats {
    pub step: i64,
    pub observed_deltas: u64,
    pub min_residual: i64,
    pub max_residual: i64,
    pub max_abs_residual: u64,
    pub max_gap_steps: u64,
    pub gap_count: u64,
}

impl RoughStepStats {
    pub fn new(step: i64) -> Self {
        Self {
            step,
            observed_deltas: 1,
            min_residual: 0,
            max_residual: 0,
            max_abs_residual: 0,
            max_gap_steps: 1,
            gap_count: 0,
        }
    }

    pub fn observe_delta(&mut self, delta: i64) {
        let step_count = nearest_step_count(delta, self.step);
        let residual = delta - self.step.saturating_mul(step_count);
        self.observed_deltas += 1;
        self.min_residual = self.min_residual.min(residual);
        self.max_residual = self.max_residual.max(residual);
        self.max_abs_residual = self.max_abs_residual.max(residual.unsigned_abs());

        let gap_steps = step_count.unsigned_abs();
        self.max_gap_steps = self.max_gap_steps.max(gap_steps);
        if gap_steps > 1 {
            self.gap_count += gap_steps - 1;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldStatsSummary {
    pub field_index: u16,
    pub observed: u64,
    pub min: i64,
    pub max: i64,
    pub max_abs_delta: u64,
    pub delta_valid: bool,
    pub monotonic_non_decreasing: bool,
    pub first_value: Option<i64>,
    pub fixed_step: Option<i64>,
    pub fixed_step_valid: bool,
    pub delta2_valid: bool,
}

impl PartialEq for FieldStats {
    fn eq(&self, other: &Self) -> bool {
        self.field_index == other.field_index
            && self.observed == other.observed
            && self.min == other.min
            && self.max == other.max
            && self.max_abs_delta == other.max_abs_delta
            && self.delta_valid == other.delta_valid
            && self.monotonic_non_decreasing == other.monotonic_non_decreasing
            && self.first_value == other.first_value
            && self.fixed_step == other.fixed_step
            && self.fixed_step_valid == other.fixed_step_valid
            && self.delta2_valid == other.delta2_valid
    }
}

impl FieldStats {
    pub const fn new(field_index: u16) -> Self {
        Self {
            field_index,
            observed: 0,
            min: 0,
            max: 0,
            first_delta: None,
            min_delta: 0,
            max_delta: 0,
            max_abs_delta: 0,
            delta_valid: true,
            min_delta2: 0,
            max_delta2: 0,
            max_abs_delta2: 0,
            delta2_valid: true,
            monotonic_non_decreasing: true,
            first_value: None,
            fixed_step: None,
            fixed_step_valid: true,
            rough_step: None,
            absolute_zigzag_varint_bytes: 0,
            previous_delta_zigzag_varint_bytes: 0,
            delta2_zigzag_varint_bytes: 0,
            previous: None,
            previous_delta: None,
        }
    }

    pub fn observe(&mut self, value: i64) {
        if self.observed == 0 {
            self.min = value;
            self.max = value;
            self.first_value = Some(value);
            self.previous_delta_zigzag_varint_bytes = self
                .previous_delta_zigzag_varint_bytes
                .saturating_add(u64::from(zigzag_varint_len(value)));
            self.delta2_zigzag_varint_bytes = self
                .delta2_zigzag_varint_bytes
                .saturating_add(u64::from(zigzag_varint_len(value)));
        } else {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
        }
        self.absolute_zigzag_varint_bytes += u64::from(zigzag_varint_len(value));

        if let Some(previous) = self.previous {
            let raw_step = i128::from(value) - i128::from(previous);
            self.max_abs_delta = self.max_abs_delta.max(raw_step.unsigned_abs() as u64);
            if let Ok(step) = i64::try_from(raw_step) {
                if self.first_delta.is_none() {
                    self.first_delta = Some(step);
                    self.min_delta = step;
                    self.max_delta = step;
                    self.rough_step = Some(RoughStepStats::new(step));
                    self.delta2_zigzag_varint_bytes = self
                        .delta2_zigzag_varint_bytes
                        .saturating_add(u64::from(zigzag_varint_len(step)));
                } else {
                    self.min_delta = self.min_delta.min(step);
                    self.max_delta = self.max_delta.max(step);
                    if let Some(rough_step) = &mut self.rough_step {
                        rough_step.observe_delta(step);
                    }
                }

                self.previous_delta_zigzag_varint_bytes = self
                    .previous_delta_zigzag_varint_bytes
                    .saturating_add(u64::from(zigzag_varint_len(step)));

                if let Some(previous_delta) = self.previous_delta {
                    if let Some(delta2) = step.checked_sub(previous_delta) {
                        if self.observed == 2 {
                            self.min_delta2 = delta2;
                            self.max_delta2 = delta2;
                        } else {
                            self.min_delta2 = self.min_delta2.min(delta2);
                            self.max_delta2 = self.max_delta2.max(delta2);
                        }
                        self.max_abs_delta2 = self.max_abs_delta2.max(delta2.unsigned_abs());
                        self.delta2_zigzag_varint_bytes = self
                            .delta2_zigzag_varint_bytes
                            .saturating_add(u64::from(zigzag_varint_len(delta2)));
                    } else {
                        self.delta2_valid = false;
                        self.delta2_zigzag_varint_bytes = u64::MAX;
                    }
                }

                if self.delta_valid {
                    if self.observed == 1 {
                        self.fixed_step = Some(step);
                    } else if self.fixed_step != Some(step) {
                        self.fixed_step_valid = false;
                    }
                }
                self.previous_delta = Some(step);
            } else {
                self.delta_valid = false;
                self.delta2_valid = false;
                self.fixed_step_valid = false;
                self.previous_delta = None;
                self.previous_delta_zigzag_varint_bytes = u64::MAX;
                self.delta2_zigzag_varint_bytes = u64::MAX;
            }
            if value < previous {
                self.monotonic_non_decreasing = false;
            }
        }

        self.previous = Some(value);
        self.observed += 1;
    }

    pub const fn from_summary(summary: FieldStatsSummary) -> Self {
        Self {
            field_index: summary.field_index,
            observed: summary.observed,
            min: summary.min,
            max: summary.max,
            max_abs_delta: summary.max_abs_delta,
            delta_valid: summary.delta_valid,
            monotonic_non_decreasing: summary.monotonic_non_decreasing,
            first_value: summary.first_value,
            fixed_step: summary.fixed_step,
            fixed_step_valid: summary.fixed_step_valid,
            first_delta: None,
            min_delta: 0,
            max_delta: 0,
            min_delta2: 0,
            max_delta2: 0,
            max_abs_delta2: 0,
            delta2_valid: summary.delta2_valid,
            rough_step: None,
            absolute_zigzag_varint_bytes: 0,
            previous_delta_zigzag_varint_bytes: 0,
            delta2_zigzag_varint_bytes: 0,
            previous: None,
            previous_delta: None,
        }
    }

    pub fn absolute_width(&self) -> PhysicalWidth {
        signed_width_for_range(self.min, self.max)
    }

    pub fn delta_width(&self) -> PhysicalWidth {
        signed_width_for_abs_delta(self.max_abs_delta)
    }

    pub fn delta2_width(&self) -> PhysicalWidth {
        signed_width_for_range(self.min_delta2, self.max_delta2)
    }

    pub fn base_value(&self) -> i64 {
        self.min
    }

    pub fn max_abs_base_delta(&self) -> u64 {
        abs_delta(self.min, self.max)
    }

    pub fn base_delta_width(&self) -> PhysicalWidth {
        signed_width_for_abs_delta(self.max_abs_base_delta())
    }

    pub fn midpoint_value(&self) -> i64 {
        ((i128::from(self.min) + i128::from(self.max)) / 2) as i64
    }

    pub fn max_abs_midpoint_delta(&self) -> u64 {
        let midpoint = self.midpoint_value();
        abs_delta(midpoint, self.min).max(abs_delta(midpoint, self.max))
    }

    pub fn midpoint_delta_width(&self) -> PhysicalWidth {
        signed_width_for_abs_delta(self.max_abs_midpoint_delta())
    }

    pub fn has_implicit_fixed_step(&self) -> bool {
        self.observed >= 2 && self.fixed_step_valid && self.fixed_step.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedFieldStats {
    pub field_index: u16,
    pub related_field_index: u16,
    pub observed: u64,
    pub min_delta: i64,
    pub max_delta: i64,
    pub max_abs_delta: u64,
    pub delta_valid: bool,
}

impl RelatedFieldStats {
    pub const fn new(field_index: u16, related_field_index: u16) -> Self {
        Self {
            field_index,
            related_field_index,
            observed: 0,
            min_delta: 0,
            max_delta: 0,
            max_abs_delta: 0,
            delta_valid: true,
        }
    }

    pub fn observe(&mut self, value: i64, related_value: i64) {
        let raw_delta = i128::from(value) - i128::from(related_value);
        self.max_abs_delta = self.max_abs_delta.max(raw_delta.unsigned_abs() as u64);
        if let Ok(delta) = i64::try_from(raw_delta) {
            if self.observed == 0 {
                self.min_delta = delta;
                self.max_delta = delta;
            } else {
                self.min_delta = self.min_delta.min(delta);
                self.max_delta = self.max_delta.max(delta);
            }
        } else {
            self.delta_valid = false;
        }
        self.observed += 1;
    }

    pub fn delta_width(&self) -> PhysicalWidth {
        signed_width_for_range(self.min_delta, self.max_delta)
    }
}

/// Timestamp grouping shape tracked during ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunHistogramEntry {
    pub run_len: u32,
    pub count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShapeStats {
    pub max_records_per_timestamp: u32,
    pub timestamp_run_histogram: Vec<RunHistogramEntry>,
}

impl ShapeStats {
    pub fn observe_timestamp_run(&mut self, run_len: u32) {
        if run_len == 0 {
            return;
        }
        self.max_records_per_timestamp = self.max_records_per_timestamp.max(run_len);
        match self
            .timestamp_run_histogram
            .binary_search_by_key(&run_len, |entry| entry.run_len)
        {
            Ok(idx) => self.timestamp_run_histogram[idx].count += 1,
            Err(idx) => self
                .timestamp_run_histogram
                .insert(idx, RunHistogramEntry { run_len, count: 1 }),
        }
    }
}

/// Seal-time stats collected while writing a normalized `.aura` ingest file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestStats {
    pub record_count: u64,
    pub fields: Vec<FieldStats>,
    pub related_fields: Vec<RelatedFieldStats>,
    pub shape: ShapeStats,
}

impl IngestStats {
    pub fn new(field_count: usize) -> Result<Self> {
        if field_count > u16::MAX as usize {
            return Err(AuraError::InvalidValue("field count"));
        }
        Ok(Self {
            record_count: 0,
            fields: (0..field_count)
                .map(|idx| FieldStats::new(idx as u16))
                .collect(),
            related_fields: Vec::new(),
            shape: ShapeStats::default(),
        })
    }

    pub fn new_for_schema(schema: &SchemaDescriptor) -> Result<Self> {
        let mut stats = Self::new(schema.fields.len())?;
        for field in &schema.fields {
            if let FieldRelation::DeltaFromField(related_field_index) = field.relation {
                stats
                    .related_fields
                    .push(RelatedFieldStats::new(field.index, related_field_index));
            }
        }
        Ok(stats)
    }

    pub fn observe_record(&mut self) {
        self.record_count += 1;
    }

    pub fn observe_i64(&mut self, field_index: u16, value: i64) -> Result<()> {
        let field = self
            .fields
            .get_mut(usize::from(field_index))
            .ok_or(AuraError::InvalidValue("field index"))?;
        field.observe(value);
        Ok(())
    }

    pub fn observe_i64_record(&mut self, schema: &SchemaDescriptor, values: &[i64]) -> Result<()> {
        if values.len() != schema.fields.len() {
            return Err(AuraError::InvalidValue("record field count"));
        }
        self.observe_record();
        for field in &schema.fields {
            self.observe_i64(field.index, values[usize::from(field.index)])?;
        }
        for related in &mut self.related_fields {
            related.observe(
                values[usize::from(related.field_index)],
                values[usize::from(related.related_field_index)],
            );
        }
        Ok(())
    }

    pub fn observe_timestamp_run(&mut self, run_len: u32) {
        self.shape.observe_timestamp_run(run_len);
    }

    pub fn field(&self, field_index: u16) -> Option<&FieldStats> {
        self.fields.get(usize::from(field_index))
    }

    pub fn related_field(&self, field_index: u16) -> Option<&RelatedFieldStats> {
        self.related_fields
            .iter()
            .find(|related| related.field_index == field_index)
    }
}

pub const fn signed_width_for_range(min: i64, max: i64) -> PhysicalWidth {
    if min >= i8::MIN as i64 && max <= i8::MAX as i64 {
        PhysicalWidth::I8
    } else if min >= i16::MIN as i64 && max <= i16::MAX as i64 {
        PhysicalWidth::I16
    } else if min >= i32::MIN as i64 && max <= i32::MAX as i64 {
        PhysicalWidth::I32
    } else {
        PhysicalWidth::I64
    }
}

pub const fn signed_width_for_abs_delta(max_abs_delta: u64) -> PhysicalWidth {
    if max_abs_delta <= i8::MAX as u64 {
        PhysicalWidth::I8
    } else if max_abs_delta <= i16::MAX as u64 {
        PhysicalWidth::I16
    } else if max_abs_delta <= i32::MAX as u64 {
        PhysicalWidth::I32
    } else {
        PhysicalWidth::I64
    }
}

fn abs_delta(previous: i64, value: i64) -> u64 {
    let delta = i128::from(value) - i128::from(previous);
    delta.unsigned_abs() as u64
}

fn nearest_step_count(delta: i64, step: i64) -> i64 {
    if step == 0 {
        return 1;
    }
    let rounded = (delta as f64 / step as f64).round() as i64;
    if rounded == 0 {
        1
    } else {
        rounded
    }
}

pub fn signed_bit_width_for_range(min: i64, max: i64) -> u8 {
    if min >= 0 {
        return unsigned_bit_width(max as u64);
    }
    for bits in 1..=64 {
        let lower = -(1i128 << (bits - 1));
        let upper = (1i128 << (bits - 1)) - 1;
        if i128::from(min) >= lower && i128::from(max) <= upper {
            return bits as u8;
        }
    }
    64
}

pub fn unsigned_bit_width(value: u64) -> u8 {
    if value == 0 {
        0
    } else {
        (u64::BITS - value.leading_zeros()) as u8
    }
}

pub fn zigzag_varint_len(value: i64) -> u8 {
    unsigned_varint_len(zigzag_i64(value))
}

pub fn unsigned_varint_len(mut value: u64) -> u8 {
    let mut bytes = 1;
    while value >= 0x80 {
        value >>= 7;
        bytes += 1;
    }
    bytes
}

fn zigzag_i64(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

/// Seal-time summary used to choose hot layouts without decoding twice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventStats {
    pub event_count: u64,
    pub bid_level_count: u64,
    pub ask_level_count: u64,
    pub max_bid_count: u32,
    pub max_ask_count: u32,
    pub first_ts_event: Option<u64>,
    pub last_ts_event: Option<u64>,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
}

impl EventStats {
    pub fn observe(&mut self, event: &BookEvent) {
        let bid_count = event.bids.len() as u32;
        let ask_count = event.asks.len() as u32;
        self.event_count += 1;
        self.bid_level_count += u64::from(bid_count);
        self.ask_level_count += u64::from(ask_count);
        self.max_bid_count = self.max_bid_count.max(bid_count);
        self.max_ask_count = self.max_ask_count.max(ask_count);
        self.first_ts_event.get_or_insert(event.ts_event);
        self.last_ts_event = Some(event.ts_event);
        self.first_sequence.get_or_insert(event.sequence);
        self.last_sequence = Some(event.sequence);
    }

    pub fn from_events(events: &[BookEvent]) -> Self {
        let mut stats = Self::default();
        for event in events {
            stats.observe(event);
        }
        stats
    }

    pub fn level_count(&self) -> u64 {
        self.bid_level_count + self.ask_level_count
    }

    pub fn padded_slots(&self, block_size: u16) -> u64 {
        if block_size == 0 {
            return self.level_count();
        }
        let block = u64::from(block_size);
        round_up(self.bid_level_count, block) + round_up(self.ask_level_count, block)
    }
}

pub const fn round_up(value: u64, block: u64) -> u64 {
    if block == 0 || value == 0 {
        value
    } else {
        value.div_ceil(block) * block
    }
}

pub fn padded_event_slots(count: usize, block_size: u16) -> usize {
    if block_size == 0 {
        return count;
    }
    let block = usize::from(block_size);
    if count == 0 {
        0
    } else {
        count.div_ceil(block) * block
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BookEvent, BookId, LevelChange};

    #[test]
    fn stats_track_counts_and_edges() {
        let events = vec![
            BookEvent::new(
                100,
                7,
                BookId::BookA,
                vec![LevelChange::new(10, 1, 0)],
                vec![],
            ),
            BookEvent::new(
                200,
                8,
                BookId::BookA,
                vec![LevelChange::new(11, 2, 0), LevelChange::new(12, 3, 0)],
                vec![LevelChange::new(13, 4, 0)],
            ),
        ];

        let stats = EventStats::from_events(&events);

        assert_eq!(2, stats.event_count);
        assert_eq!(3, stats.bid_level_count);
        assert_eq!(1, stats.ask_level_count);
        assert_eq!(2, stats.max_bid_count);
        assert_eq!(Some(100), stats.first_ts_event);
        assert_eq!(Some(200), stats.last_ts_event);
    }

    #[test]
    fn padding_rounds_to_blocks() {
        assert_eq!(0, padded_event_slots(0, 8));
        assert_eq!(8, padded_event_slots(1, 8));
        assert_eq!(8, padded_event_slots(8, 8));
        assert_eq!(16, padded_event_slots(9, 8));
    }

    #[test]
    fn ingest_stats_track_ranges_deltas_and_shapes() {
        let mut stats = IngestStats::new(2).unwrap();
        stats.observe_record();
        stats.observe_i64(0, 100).unwrap();
        stats.observe_i64(1, -5).unwrap();
        stats.observe_record();
        stats.observe_i64(0, 104).unwrap();
        stats.observe_i64(1, 120).unwrap();
        stats.observe_timestamp_run(4);
        stats.observe_timestamp_run(4);
        stats.observe_timestamp_run(7);

        let field = stats.field(0).unwrap();
        assert_eq!(2, stats.record_count);
        assert_eq!(100, field.min);
        assert_eq!(104, field.max);
        assert_eq!(4, field.max_abs_delta);
        assert_eq!(PhysicalWidth::I8, field.absolute_width());
        assert_eq!(PhysicalWidth::I8, field.delta_width());
        assert_eq!(7, stats.shape.max_records_per_timestamp);
        assert_eq!(
            vec![
                RunHistogramEntry {
                    run_len: 4,
                    count: 2
                },
                RunHistogramEntry {
                    run_len: 7,
                    count: 1
                }
            ],
            stats.shape.timestamp_run_histogram
        );
    }

    #[test]
    fn width_helpers_promote_when_values_need_more_bytes() {
        assert_eq!(PhysicalWidth::I8, signed_width_for_range(-10, 10));
        assert_eq!(PhysicalWidth::I16, signed_width_for_range(-200, 10));
        assert_eq!(PhysicalWidth::I32, signed_width_for_abs_delta(70_000));
    }
}
