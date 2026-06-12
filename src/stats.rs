use crate::types::BookEvent;

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
        ((value + block - 1) / block) * block
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
        ((count + block - 1) / block) * block
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
}
