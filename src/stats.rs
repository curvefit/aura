use crate::types::BookEvent;
use crate::{AuraError, Result};

/// Smallest fixed integer width proven safe by ingest stats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PhysicalWidth {
    I8,
    I16,
    I32,
    I64,
}

impl PhysicalWidth {
    pub const fn byte_width(self) -> u8 {
        match self {
            Self::I8 => 1,
            Self::I16 => 2,
            Self::I32 => 4,
            Self::I64 => 8,
        }
    }

    pub const fn code(self) -> u8 {
        match self {
            Self::I8 => 1,
            Self::I16 => 2,
            Self::I32 => 3,
            Self::I64 => 4,
        }
    }

    pub fn from_code(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::I8),
            2 => Ok(Self::I16),
            3 => Ok(Self::I32),
            4 => Ok(Self::I64),
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
    pub max_abs_delta: u64,
    pub monotonic_non_decreasing: bool,
    previous: Option<i64>,
}

impl PartialEq for FieldStats {
    fn eq(&self, other: &Self) -> bool {
        self.field_index == other.field_index
            && self.observed == other.observed
            && self.min == other.min
            && self.max == other.max
            && self.max_abs_delta == other.max_abs_delta
            && self.monotonic_non_decreasing == other.monotonic_non_decreasing
    }
}

impl FieldStats {
    pub const fn new(field_index: u16) -> Self {
        Self {
            field_index,
            observed: 0,
            min: 0,
            max: 0,
            max_abs_delta: 0,
            monotonic_non_decreasing: true,
            previous: None,
        }
    }

    pub fn observe(&mut self, value: i64) {
        if self.observed == 0 {
            self.min = value;
            self.max = value;
        } else {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
        }

        if let Some(previous) = self.previous {
            self.max_abs_delta = self.max_abs_delta.max(abs_delta(previous, value));
            if value < previous {
                self.monotonic_non_decreasing = false;
            }
        }

        self.previous = Some(value);
        self.observed += 1;
    }

    pub const fn from_summary(
        field_index: u16,
        observed: u64,
        min: i64,
        max: i64,
        max_abs_delta: u64,
        monotonic_non_decreasing: bool,
    ) -> Self {
        Self {
            field_index,
            observed,
            min,
            max,
            max_abs_delta,
            monotonic_non_decreasing,
            previous: None,
        }
    }

    pub fn absolute_width(&self) -> PhysicalWidth {
        signed_width_for_range(self.min, self.max)
    }

    pub fn delta_width(&self) -> PhysicalWidth {
        signed_width_for_abs_delta(self.max_abs_delta)
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
            shape: ShapeStats::default(),
        })
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

    pub fn observe_timestamp_run(&mut self, run_len: u32) {
        self.shape.observe_timestamp_run(run_len);
    }

    pub fn field(&self, field_index: u16) -> Option<&FieldStats> {
        self.fields.get(usize::from(field_index))
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
